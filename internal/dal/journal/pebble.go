package journal

import (
	"bytes"
	"cmp"
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/cockroachdb/pebble/v2"
	"github.com/cockroachdb/pebble/v2/vfs"
)

const mirrorSchemaVersion = "1"

const mirrorOpenWait = 5 * time.Second

type mirrorDB struct {
	actor *mirrorActor
	path  string
	db    *pebble.DB
	guard *mirrorGuard
}

type mirrorActor struct {
	mu     sync.Mutex
	req    chan func() error
	res    chan error
	done   chan struct{}
	closed bool
}

func (m *mirrorDB) apply(fn func(*pebble.DB) error) error {
	if m.db != nil {
		return fn(m.db)
	}
	guard, err := lockMirrorDir(m.path)
	if err != nil {
		return fmt.Errorf("journal: open state.db: %w", err)
	}
	db, err := openMirror(m.path, guard.lock)
	if err != nil {
		_ = guard.Close()
		return fmt.Errorf("journal: open state.db: %w", err)
	}
	defer func() { _ = guard.Close() }()
	defer func() { _ = db.Close() }()
	return fn(db)
}

func spawnMirrorActor(m *mirrorDB) *mirrorActor {
	actor := &mirrorActor{
		req:  make(chan func() error),
		res:  make(chan error),
		done: make(chan struct{}),
	}
	go func() {
		defer close(actor.done)
		for fn := range actor.req {
			actor.res <- fn()
		}
	}()
	return actor
}

var errMirrorClosed = errors.New("journal: state.db is closed")

func (a *mirrorActor) call(fn func() error) error {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.closed {
		return errMirrorClosed
	}
	a.req <- fn
	return <-a.res
}

func (a *mirrorActor) shutdown() {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.closed {
		return
	}
	a.closed = true
	close(a.req)
	<-a.done
}

func (m *mirrorDB) Close() error {
	if m == nil || m.actor == nil {
		return nil
	}
	err := m.dropDB(m.actor)
	m.actor.shutdown()
	return err
}

func (m *mirrorDB) dropDB(actor *mirrorActor) error {
	err := actor.call(func() error {
		db, guard := m.db, m.guard
		m.db, m.guard = nil, nil
		var result error
		if db != nil {
			result = db.Close()
		}
		if guard != nil {
			result = errors.Join(result, guard.Close())
		}
		return result
	})
	if errors.Is(err, errMirrorClosed) {
		return nil
	}
	return err
}

func (m *mirrorDB) detach() {
	_ = m.dropDB(m.actor)
}

type mirrorLogger struct{}

func (mirrorLogger) Infof(string, ...any)  {}
func (mirrorLogger) Fatalf(string, ...any) {}
func (mirrorLogger) Errorf(string, ...any) {}

var errMirrorBusy = errors.New("journal: state.db is busy")

type mirrorGuard struct {
	lock *pebble.Lock
	file *os.File
}

func (g *mirrorGuard) Close() error {
	return errors.Join(g.lock.Close(), releaseGate(g.file))
}

func lockMirrorDir(path string) (*mirrorGuard, error) {
	if err := os.MkdirAll(path, 0o700); err != nil {
		return nil, err
	}
	gate, err := lockGateBounded(path+".lock", mirrorOpenWait)
	if err != nil {
		return nil, err
	}
	lock, err := pebble.LockDirectory(path, vfs.Default)
	if err != nil {
		return nil, errors.Join(err, releaseGate(gate))
	}
	return &mirrorGuard{lock: lock, file: gate}, nil
}

func openMirror(path string, lock *pebble.Lock) (*pebble.DB, error) {
	return pebble.Open(path, &pebble.Options{Lock: lock, Logger: mirrorLogger{}})
}

func (m *mirrorDB) withDB(fn func(*pebble.DB) error) error {
	if m == nil || m.actor == nil {
		return errMirrorClosed
	}
	return m.actor.call(func() error { return m.apply(fn) })
}

func openDB(path string) (*mirrorDB, error) {
	guard, err := lockMirrorDir(path)
	if err != nil {
		return nil, fmt.Errorf("journal: open state.db: %w", err)
	}
	db, err := openMirror(path, guard.lock)
	if err != nil {
		return nil, errors.Join(err, guard.Close())
	}
	if err := checkMirrorVersion(db); err != nil {
		return nil, errors.Join(err, db.Close(), guard.Close())
	}
	m := &mirrorDB{db: db, guard: guard, path: path}
	m.actor = spawnMirrorActor(m)
	return m, nil
}

var keyVersion = []byte("v")

func checkMirrorVersion(db *pebble.DB) error {
	value, closer, err := db.Get(keyVersion)
	switch {
	case errors.Is(err, pebble.ErrNotFound):
		return db.Set(keyVersion, []byte(mirrorSchemaVersion), pebble.Sync)
	case err != nil:
		return err
	}
	defer func() { _ = closer.Close() }()
	if string(value) != mirrorSchemaVersion {
		return fmt.Errorf("journal: state.db version %s exceeds supported version %s", value, mirrorSchemaVersion)
	}
	return nil
}

func openUsableDB(path string) (*mirrorDB, error) {
	m, err := openDB(path)
	if err != nil {
		return nil, err
	}
	if err := m.verify(); err != nil {
		return nil, errors.Join(err, m.Close())
	}
	return m, nil
}

func (m *mirrorDB) verify() error {
	return m.withDB(func(db *pebble.DB) error {
		it, err := db.NewIter(nil)
		if err != nil {
			return err
		}
		defer func() { _ = it.Close() }()
		for valid := it.First(); valid; valid = it.Next() {
		}
		return it.Error()
	})
}

func backupSources(dir string) ([]string, error) {
	var sources []string
	for _, name := range []string{"state.db.bak", "state.db.bak.old"} {
		candidate := filepath.Join(dir, name)
		if _, err := os.Stat(candidate); err == nil {
			sources = append(sources, candidate)
		} else if !errors.Is(err, os.ErrNotExist) {
			return nil, err
		}
	}
	tmps, err := tmpBackupCandidates(dir)
	if err != nil {
		return nil, err
	}
	return append(sources, tmps...), nil
}

func recoverBackup(dir string) (*mirrorDB, error) {
	sources, err := backupSources(dir)
	if err != nil {
		return nil, err
	}
	var result error
	for _, source := range sources {
		db, err := restoreBackupSource(dir, source)
		if err == nil {
			return db, nil
		}
		result = errors.Join(result, err)
	}
	return nil, result
}

func restoreBackupSource(dir, source string) (*mirrorDB, error) {
	tmp := filepath.Join(dir, "state.db.recover.tmp")
	path := filepath.Join(dir, "state.db")
	if err := os.RemoveAll(tmp); err != nil {
		return nil, err
	}
	if err := os.RemoveAll(path); err != nil {
		return nil, err
	}
	if err := copyMirrorDir(source, tmp); err != nil {
		return nil, err
	}
	if err := os.Rename(tmp, path); err != nil {
		return nil, err
	}
	if err := syncDir(dir); err != nil {
		return nil, err
	}
	return openUsableDB(path)
}

func copyMirrorDir(source, destination string) (retErr error) {
	return filepath.WalkDir(source, func(path string, entry os.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		rel, err := filepath.Rel(source, path)
		if err != nil {
			return err
		}
		target := filepath.Join(destination, rel)
		if entry.IsDir() {
			return os.MkdirAll(target, 0o700)
		}
		return copyMirrorFile(path, target)
	})
}

func copyMirrorFile(source, destination string) (retErr error) {
	src, err := os.Open(source)
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, src.Close()) }()
	dst, err := os.OpenFile(destination, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o600)
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, dst.Close()) }()
	if _, err = io.Copy(dst, src); err != nil {
		return err
	}
	return dst.Sync()
}

type sessIndex struct {
	hashes map[uint64]string
	head   uint64
}

type dbIndex struct {
	events   map[string]*sessIndex
	sessions map[string]sessionRow
}

type sessionRow struct {
	status  string
	headSeq uint64
}

func loadSessionRows(m *mirrorDB) (*dbIndex, error) {
	idx := &dbIndex{
		events:   map[string]*sessIndex{},
		sessions: map[string]sessionRow{},
	}
	if err := m.withDB(func(db *pebble.DB) error {
		return loadSessionIndex(db, idx.sessions)
	}); err != nil {
		return nil, err
	}
	return idx, nil
}

func loadSessionEvents(m *mirrorDB, sid string) (_ *sessIndex, retErr error) {
	events := map[string]*sessIndex{}
	err := m.withDB(func(db *pebble.DB) error {
		return scanSessionEvents(db, sid, func(seq uint64, hash string) {
			indexEvent(events, sid, seq, hash)
		})
	})
	if err != nil {
		return nil, err
	}
	return events[sid], nil
}

func indexEvent(events map[string]*sessIndex, sid string, seq uint64, hash string) {
	si := events[sid]
	if si == nil {
		si = &sessIndex{hashes: map[uint64]string{}}
		events[sid] = si
	}
	si.hashes[seq] = hash
	si.head = max(si.head, seq)
}

func lenPrefix(s string) []byte {
	prefix := make([]byte, 2+len(s))
	binary.BigEndian.PutUint16(prefix, uint16(len(s))) //nolint:gosec // session ids are bounded by callers
	copy(prefix[2:], s)
	return prefix
}

func seqBytes(seq uint64) []byte {
	var buf [8]byte
	binary.BigEndian.PutUint64(buf[:], seq)
	return buf[:]
}

func keySession(id string) []byte {
	return append([]byte("s"), lenPrefix(id)...)
}

func keyEvent(sid string, seq uint64) []byte {
	key := append([]byte("e"), lenPrefix(sid)...)
	return append(key, seqBytes(seq)...)
}

func keyIndex(sid string, seq uint64) []byte {
	key := append([]byte("i"), lenPrefix(sid)...)
	return append(key, seqBytes(seq)...)
}

func keyPosting(trigram, sid string, seq uint64) []byte {
	key := append([]byte("t"), trigram...)
	key = append(key, '\x1f')
	key = append(key, lenPrefix(sid)...)
	return append(key, seqBytes(seq)...)
}

func upperBound(prefix []byte) []byte {
	upper := bytes.Clone(prefix)
	for i := len(upper) - 1; i >= 0; i-- {
		if upper[i] != 0xff {
			upper[i]++
			return upper[:i+1]
		}
	}
	return nil
}

func sessionPrefix(kind byte, sid string) []byte {
	return append([]byte{kind}, lenPrefix(sid)...)
}

func scanSessionEvents(db *pebble.DB, sid string, fn func(seq uint64, hash string)) error {
	prefix := sessionPrefix('e', sid)
	it, err := db.NewIter(&pebble.IterOptions{LowerBound: prefix, UpperBound: upperBound(prefix)})
	if err != nil {
		return err
	}
	defer func() { _ = it.Close() }()
	for valid := it.First(); valid; valid = it.Next() {
		key := it.Key()
		seq := binary.BigEndian.Uint64(key[len(key)-8:])
		hash, _, _ := bytes.Cut(it.Value(), []byte{0})
		fn(seq, string(hash))
	}
	return it.Error()
}

func loadSessionIndex(db *pebble.DB, sessions map[string]sessionRow) (retErr error) {
	it, err := db.NewIter(&pebble.IterOptions{LowerBound: []byte("s"), UpperBound: []byte("t")})
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, it.Close()) }()
	for valid := it.First(); valid; valid = it.Next() {
		key := it.Key()[1:]
		id := string(key[2 : 2+binary.BigEndian.Uint16(key)])
		sessions[id] = coerceSessionRow(it.Value())
	}
	return it.Error()
}

func coerceSessionRow(value []byte) sessionRow {
	status, rest, _ := strings.Cut(string(value), "\x1f")
	head, _, _ := strings.Cut(rest, "\x1f")
	row := sessionRow{status: status}
	if row.status != string(SessionOK) && row.status != string(SessionDamaged) {
		row.status = string(SessionDamaged)
	}
	seq, err := strconv.ParseInt(head, 10, 64)
	if err != nil || seq < 0 {
		seq = 0
	}
	row.headSeq = uint64(seq) //nolint:gosec // seq is clamped to >= 0
	return row
}

type fetchResult struct {
	hash string
	ev   []byte
	ok   bool
}

func fetchRow(m *mirrorDB, sid string, seq uint64) (fetchResult, error) {
	var result fetchResult
	err := m.withDB(func(db *pebble.DB) error {
		value, closer, err := db.Get(keyEvent(sid, seq))
		if errors.Is(err, pebble.ErrNotFound) {
			return nil
		}
		if err != nil {
			return err
		}
		defer func() { _ = closer.Close() }()
		hash, ev, _ := bytes.Cut(value, []byte{0})
		result = fetchResult{hash: string(hash), ev: bytes.Clone(ev), ok: true}
		return nil
	})
	return result, err
}

func trigrams(body string) map[string]struct{} {
	set := map[string]struct{}{}
	runes := []rune(strings.ToLower(body))
	for i := 0; i+3 <= len(runes); i++ {
		set[string(runes[i:i+3])] = struct{}{}
	}
	return set
}

func setSessionRow(batch *pebble.Batch, db *pebble.DB, sid, status string, headSeq uint64) error {
	value, closer, err := db.Get(keySession(sid))
	switch {
	case err == nil:
		prior := coerceSessionRow(value)
		_ = closer.Close()
		if prior.status == string(SessionDamaged) {
			status = string(SessionDamaged)
		}
	case !errors.Is(err, pebble.ErrNotFound):
		return err
	}
	row := status + "\x1f" + strconv.FormatUint(headSeq, 10) + "\x1f" + time.Now().UTC().Format(time.RFC3339)
	return batch.Set(keySession(sid), []byte(row), nil)
}

func insertEvents(batch *pebble.Batch, db *pebble.DB, sid string, recs []emitRec) error {
	for _, rec := range recs {
		if err := batch.Set(keyEvent(sid, rec.seq), append([]byte(rec.hash), append([]byte{0}, rec.ev...)...), nil); err != nil {
			return err
		}
		kind, body, ok := searchText(rec.ev)
		if !ok {
			continue
		}
		if err := batch.Set(keyIndex(sid, rec.seq), []byte(kind+"\x00"+body), nil); err != nil {
			return err
		}
		for trigram := range trigrams(body) {
			if err := batch.Set(keyPosting(trigram, sid, rec.seq), nil, nil); err != nil {
				return err
			}
		}
	}
	return nil
}

func mirrorAppend(m *mirrorDB, sid string, recs []emitRec, headSeq uint64) error {
	return m.withDB(func(db *pebble.DB) error {
		batch := db.NewBatch()
		defer func() { _ = batch.Close() }()
		if err := insertEvents(batch, db, sid, recs); err != nil {
			return err
		}
		if err := setSessionRow(batch, db, sid, string(SessionOK), headSeq); err != nil {
			return err
		}
		return batch.Commit(pebble.Sync)
	})
}

func deleteSessionRows(batch *pebble.Batch, db *pebble.DB, sid string) error {
	prefix := sessionPrefix('i', sid)
	it, err := db.NewIter(&pebble.IterOptions{LowerBound: prefix, UpperBound: upperBound(prefix)})
	if err != nil {
		return err
	}
	defer func() { _ = it.Close() }()
	for valid := it.First(); valid; valid = it.Next() {
		key := it.Key()
		seq := binary.BigEndian.Uint64(key[len(key)-8:])
		_, body, _ := bytes.Cut(it.Value(), []byte{0})
		for trigram := range trigrams(string(body)) {
			if err := batch.Delete(keyPosting(trigram, sid, seq), nil); err != nil {
				return err
			}
		}
	}
	if err := it.Error(); err != nil {
		return err
	}
	for _, kind := range []byte{'e', 'i'} {
		prefix := sessionPrefix(kind, sid)
		if err := batch.DeleteRange(prefix, upperBound(prefix), nil); err != nil {
			return err
		}
	}
	return nil
}

func replaceSession(m *mirrorDB, sid string, recs []emitRec, head SessionHead) error {
	return m.withDB(func(db *pebble.DB) error {
		batch := db.NewBatch()
		defer func() { _ = batch.Close() }()
		if err := deleteSessionRows(batch, db, sid); err != nil {
			return err
		}
		if err := insertEvents(batch, db, sid, recs); err != nil {
			return err
		}
		if err := setSessionRow(batch, db, sid, string(head.Status), head.HeadSeq); err != nil {
			return err
		}
		return batch.Commit(pebble.Sync)
	})
}

func removeSession(m *mirrorDB, sid string) error {
	return m.withDB(func(db *pebble.DB) error {
		batch := db.NewBatch()
		defer func() { _ = batch.Close() }()
		if err := deleteSessionRows(batch, db, sid); err != nil {
			return err
		}
		if err := batch.Delete(keySession(sid), nil); err != nil {
			return err
		}
		return batch.Commit(pebble.Sync)
	})
}

func upsertSession(m *mirrorDB, sid string, status string, headSeq uint64) error {
	return m.withDB(func(db *pebble.DB) error {
		batch := db.NewBatch()
		defer func() { _ = batch.Close() }()
		if err := setSessionRow(batch, db, sid, status, headSeq); err != nil {
			return err
		}
		return batch.Commit(pebble.Sync)
	})
}

func (s *Store) backupDB() error {
	ids := s.sessionIDs()
	unlock := s.lockSessions(ids)
	defer unlock()
	s.mu.Lock()
	m, dir, dirSync := s.mirror, s.dir, s.dirSync
	s.mu.Unlock()
	if m == nil {
		return nil
	}
	return backupDBFiles(m, dir, dirSync)
}

var (
	backupPublishMu sync.Mutex
	backupStageSeq  atomic.Uint64
)

func nextBackupPath(dir, kind string) string {
	for {
		path := filepath.Join(dir, fmt.Sprintf("state.db.bak.tmp.%s.%d.%d", kind, os.Getpid(), backupStageSeq.Add(1)))
		if _, err := os.Stat(path); err != nil {
			return path
		}
	}
}

func checkpointMirror(m *mirrorDB, staging string) error {
	if err := m.withDB(func(db *pebble.DB) error {
		return db.Checkpoint(staging)
	}); err != nil {
		return fmt.Errorf("journal: backup state.db: %w", err)
	}
	return nil
}

func closeBackup(m *mirrorDB, dir string, dirSync func(string) error) (retErr error) {
	backupPublishMu.Lock()
	defer backupPublishMu.Unlock()
	staging := nextBackupPath(dir, "staged")
	defer func() { retErr = errors.Join(retErr, os.RemoveAll(staging)) }()
	if err := checkpointMirror(m, staging); err != nil {
		return err
	}
	m.detach()
	return installBackup(dir, staging, filepath.Join(dir, "state.db.bak.tmp"), dirSync)
}

func backupDBFiles(m *mirrorDB, dir string, dirSync func(string) error) (retErr error) {
	backupPublishMu.Lock()
	defer backupPublishMu.Unlock()
	staging := nextBackupPath(dir, "staged")
	defer func() { retErr = errors.Join(retErr, os.RemoveAll(staging)) }()
	if err := checkpointMirror(m, staging); err != nil {
		return err
	}
	return installBackup(dir, staging, filepath.Join(dir, "state.db.bak.tmp"), dirSync)
}

func tmpBackupCandidates(dir string) ([]string, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, fmt.Errorf("journal: list backup candidates: %w", err)
	}
	var candidates []tmpBackup
	for _, entry := range entries {
		name := entry.Name()
		if !entry.IsDir() || (name != "state.db.bak.tmp" && !strings.HasPrefix(name, "state.db.bak.tmp.staged.")) {
			continue
		}
		info, err := entry.Info()
		if err != nil {
			continue
		}
		candidates = append(candidates, tmpBackup{path: filepath.Join(dir, name), modTime: info.ModTime()})
	}
	slices.SortStableFunc(candidates, func(a, b tmpBackup) int {
		return b.modTime.Compare(a.modTime)
	})
	paths := make([]string, 0, len(candidates))
	for _, candidate := range candidates {
		paths = append(paths, candidate.path)
	}
	return paths, nil
}

type tmpBackup struct {
	path    string
	modTime time.Time
}

func retireDir(dir, path string) (string, error) {
	if _, err := os.Stat(path); err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return "", nil
		}
		return "", fmt.Errorf("journal: check stale backup: %w", err)
	}
	retired := nextBackupPath(dir, "trash")
	if err := os.Rename(path, retired); err != nil { //nolint:gosec // backup path is validated
		if errors.Is(err, os.ErrNotExist) {
			return "", nil
		}
		return "", fmt.Errorf("journal: retire stale backup: %w", err)
	}
	return retired, nil
}

func retireStaleStaging(dir, keep string) (trash []string, err error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, fmt.Errorf("journal: list stale backups: %w", err)
	}
	for _, entry := range entries {
		if !entry.IsDir() || !strings.HasPrefix(entry.Name(), "state.db.bak.tmp.") {
			continue
		}
		stale := filepath.Join(dir, entry.Name())
		if stale == keep {
			continue
		}
		retired, err := retireDir(dir, stale)
		if err != nil {
			return trash, err
		}
		if retired != "" {
			trash = append(trash, retired)
		}
	}
	return trash, nil
}

func installBackup(dir, staging, tmp string, dirSync func(string) error) (retErr error) {
	release, err := holdLock(filepath.Join(dir, "repair.lock"))
	if err != nil {
		return err
	}
	trash, lockedErr := installBackupLocked(dir, staging, tmp, dirSync)
	retErr = errors.Join(retErr, release())
	for _, path := range trash {
		retErr = errors.Join(retErr, os.RemoveAll(path))
	}
	return errors.Join(retErr, lockedErr)
}

func retireSuperseded(dir, tmp, keep string) (trash []string, retiredOld string, err error) {
	trash, err = retireStaleStaging(dir, keep)
	if err != nil {
		return trash, "", err
	}
	old := filepath.Join(dir, "state.db.bak.old")
	for _, path := range []string{tmp, old} {
		retired, rerr := retireDir(dir, path)
		if rerr != nil {
			return trash, "", rerr
		}
		if path == old {
			retiredOld = retired
		} else if retired != "" {
			trash = append(trash, retired)
		}
	}
	return trash, retiredOld, nil
}

func installBackupLocked(dir, staging, tmp string, dirSync func(string) error) (trash []string, err error) {
	old := filepath.Join(dir, "state.db.bak.old")
	trash, retiredOld, err := retireSuperseded(dir, tmp, staging)
	if err != nil {
		return trash, err
	}
	restore := func() {
		if retiredOld != "" {
			_ = os.Rename(retiredOld, old)
		}
	}
	if rerr := os.Rename(staging, tmp); rerr != nil { //nolint:gosec // backup path is validated
		restore()
		return trash, fmt.Errorf("journal: stage backup: %w", rerr)
	}
	swapped, serr := swapBackup(tmp, filepath.Join(dir, "state.db.bak"), old)
	if serr != nil {
		restore()
		return trash, serr
	}
	retiredSwap, rerr := retireDir(dir, swapped)
	if rerr != nil {
		return trash, rerr
	}
	if retiredSwap != "" {
		trash = append(trash, retiredSwap)
	}
	if retiredOld != "" {
		trash = append(trash, retiredOld)
	}
	if serr := dirSync(dir); serr != nil {
		return trash, fmt.Errorf("journal: sync backup dir %s: %w", dir, serr)
	}
	return trash, nil
}

func swapBackup(tmp, final, old string) (string, error) {
	if err := os.Rename(final, old); err != nil && !errors.Is(err, os.ErrNotExist) {
		return "", fmt.Errorf("journal: swap old backup: %w", err)
	}
	if err := os.Rename(tmp, final); err != nil { //nolint:gosec // backup path is validated
		return "", fmt.Errorf("journal: install state.db.bak: %w", err)
	}
	return old, nil
}

var (
	quarantineNow = time.Now
	quarantineSeq atomic.Uint64
)

func quarantineDB(dir, quarantine string) error {
	stem, stemErr := quarantineStem(quarantine)
	if stemErr != nil {
		return stemErr
	}
	var result error
	for _, suffix := range []string{"", "-wal", "-shm"} {
		src := filepath.Join(dir, "state.db"+suffix)
		if _, statErr := os.Stat(src); statErr != nil {
			continue
		}
		if renameErr := os.Rename(src, stem+suffix); renameErr != nil {
			result = errors.Join(result, renameErr)
		}
	}
	return result
}

func quarantineStem(quarantine string) (string, error) {
	stamp := quarantineNow().UnixNano()
	pid := os.Getpid()
	seq := quarantineSeq.Add(1)
	base := fmt.Sprintf("state-%d-%d-%d", stamp, pid, seq)
	for attempt := range 100 {
		name := base + ".db"
		if attempt > 0 {
			name = fmt.Sprintf("%s-%d.db", base, attempt)
		}
		free, freeErr := quarantineFree(quarantine, name)
		if freeErr != nil {
			return "", freeErr
		}
		if !free {
			continue
		}
		return filepath.Join(quarantine, name), nil
	}
	return "", fmt.Errorf("journal: quarantine names exhausted in %s", quarantine)
}

func quarantineFree(quarantine, name string) (bool, error) {
	for _, suffix := range []string{"", "-wal", "-shm"} {
		_, err := os.Stat(filepath.Join(quarantine, name+suffix))
		if err == nil {
			return false, nil
		}
		if !errors.Is(err, os.ErrNotExist) {
			return false, err
		}
	}
	return true, nil
}

func queryIndexRows(ctx context.Context, m *mirrorDB, terms []string) ([]SearchHit, error) {
	var rows []SearchHit
	err := m.withDB(func(db *pebble.DB) error {
		var candidates map[postingKey]struct{}
		for i, term := range terms {
			if err := ctx.Err(); err != nil {
				return err
			}
			set, err := termCandidates(db, term)
			if err != nil {
				return err
			}
			if i == 0 {
				candidates = set
				continue
			}
			intersectSet(candidates, set)
		}
		return collectIndexRows(ctx, db, candidates, terms, &rows)
	})
	return rows, err
}

func termCandidates(db *pebble.DB, term string) (map[postingKey]struct{}, error) {
	var candidates map[postingKey]struct{}
	for i, trigram := range trigramList(term) {
		set, err := postingSet(db, trigram)
		if err != nil {
			return nil, err
		}
		if i == 0 {
			candidates = set
			continue
		}
		intersectSet(candidates, set)
	}
	return candidates, nil
}

func intersectSet(candidates, set map[postingKey]struct{}) {
	for key := range candidates {
		if _, ok := set[key]; !ok {
			delete(candidates, key)
		}
	}
}

func trigramList(term string) []string {
	runes := []rune(strings.ToLower(term))
	list := make([]string, 0, len(runes))
	for i := 0; i+3 <= len(runes); i++ {
		list = append(list, string(runes[i:i+3]))
	}
	return list
}

type postingKey struct {
	sid string
	seq uint64
}

func postingSet(db *pebble.DB, trigram string) (map[postingKey]struct{}, error) {
	prefix := append([]byte("t"), trigram...)
	prefix = append(prefix, '\x1f')
	it, err := db.NewIter(&pebble.IterOptions{LowerBound: prefix, UpperBound: upperBound(prefix)})
	if err != nil {
		return nil, err
	}
	defer func() { _ = it.Close() }()
	set := map[postingKey]struct{}{}
	for valid := it.First(); valid; valid = it.Next() {
		key := it.Key()[len(prefix):]
		sidLen := int(binary.BigEndian.Uint16(key))
		sid := string(key[2 : 2+sidLen])
		seq := binary.BigEndian.Uint64(key[2+sidLen:])
		set[postingKey{sid: sid, seq: seq}] = struct{}{}
	}
	return set, it.Error()
}

func collectIndexRows(ctx context.Context, db *pebble.DB, candidates map[postingKey]struct{}, terms []string, rows *[]SearchHit) error {
	keys := make([]postingKey, 0, len(candidates))
	for key := range candidates {
		keys = append(keys, key)
	}
	slices.SortFunc(keys, func(a, b postingKey) int {
		if a.sid != b.sid {
			return strings.Compare(a.sid, b.sid)
		}
		return cmp.Compare(a.seq, b.seq)
	})
	for _, key := range keys {
		if err := ctx.Err(); err != nil {
			return err
		}
		hit, ok, err := indexRowHit(db, key, terms)
		if err != nil {
			return err
		}
		if ok {
			*rows = append(*rows, hit)
		}
	}
	return nil
}

func indexRowHit(db *pebble.DB, key postingKey, terms []string) (SearchHit, bool, error) {
	value, closer, err := db.Get(keyIndex(key.sid, key.seq))
	if err != nil {
		if errors.Is(err, pebble.ErrNotFound) {
			return SearchHit{}, false, nil
		}
		return SearchHit{}, false, err
	}
	defer func() { _ = closer.Close() }()
	kind, body, _ := bytes.Cut(value, []byte{0})
	lower := strings.ToLower(string(body))
	for _, term := range terms {
		if !strings.Contains(lower, strings.ToLower(term)) {
			return SearchHit{}, false, nil
		}
	}
	return SearchHit{Session: key.sid, Seq: key.seq, Kind: string(kind), Body: string(body)}, true, nil
}
