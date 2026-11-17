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
	lock  *pebble.Lock
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
	lock, err := lockMirrorDir(m.path)
	if err != nil {
		return fmt.Errorf("journal: open state.db: %w", err)
	}
	db, err := openMirror(m.path, lock)
	if err != nil {
		_ = lock.Close()
		return fmt.Errorf("journal: open state.db: %w", err)
	}
	defer func() { _ = lock.Close() }()
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
		db, lock := m.db, m.lock
		m.db, m.lock = nil, nil
		var result error
		if db != nil {
			result = db.Close()
		}
		if lock != nil {
			result = errors.Join(result, lock.Close())
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

func lockMirrorDir(path string) (*pebble.Lock, error) {
	if err := os.MkdirAll(path, 0o700); err != nil {
		return nil, err
	}
	deadline := time.Now().Add(mirrorOpenWait)
	for {
		lock, err := pebble.LockDirectory(path, vfs.Default)
		if err == nil {
			return lock, nil
		}
		if time.Now().After(deadline) {
			return nil, err
		}
		time.Sleep(10 * time.Millisecond)
	}
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
	lock, err := lockMirrorDir(path)
	if err != nil {
		return nil, fmt.Errorf("journal: open state.db: %w", err)
	}
	db, err := openMirror(path, lock)
	if err != nil {
		_ = lock.Close()
		return nil, fmt.Errorf("journal: open state.db: %w", err)
	}
	if err := checkMirrorVersion(db); err != nil {
		return nil, errors.Join(err, db.Close(), lock.Close())
	}
	m := &mirrorDB{db: db, lock: lock, path: path}
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

func recoverBackup(dir string) (*mirrorDB, error) {
	var source string
	for _, name := range []string{"state.db.bak", "state.db.bak.old"} {
		candidate := filepath.Join(dir, name)
		if _, err := os.Stat(candidate); err == nil {
			source = candidate
			break
		} else if !errors.Is(err, os.ErrNotExist) {
			return nil, err
		}
	}
	if source == "" {
		return nil, nil
	}
	tmp := filepath.Join(dir, "state.db.recover.tmp")
	if err := os.RemoveAll(tmp); err != nil {
		return nil, err
	}
	if err := copyMirrorDir(source, tmp); err != nil {
		return nil, err
	}
	path := filepath.Join(dir, "state.db")
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

func backupDBFiles(m *mirrorDB, dir string, dirSync func(string) error) (retErr error) {
	release, err := holdLock(filepath.Join(dir, "repair.lock"))
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, release()) }()
	tmp := filepath.Join(dir, "state.db.bak.tmp")
	final := filepath.Join(dir, "state.db.bak")
	old := filepath.Join(dir, "state.db.bak.old")
	if err := os.RemoveAll(tmp); err != nil {
		return fmt.Errorf("journal: remove stale backup: %w", err)
	}
	if err := os.RemoveAll(old); err != nil {
		return fmt.Errorf("journal: remove stale backup: %w", err)
	}
	if err := m.withDB(func(db *pebble.DB) error {
		return db.Checkpoint(tmp)
	}); err != nil {
		return fmt.Errorf("journal: backup state.db: %w", err)
	}
	if err := swapBackup(tmp, final, old); err != nil {
		return err
	}
	if err := dirSync(dir); err != nil {
		return fmt.Errorf("journal: sync backup dir %s: %w", dir, err)
	}
	return nil
}

func swapBackup(tmp, final, old string) error {
	if err := os.Rename(final, old); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("journal: swap old backup: %w", err)
	}
	if err := os.Rename(tmp, final); err != nil { //nolint:gosec // backup path is validated
		return fmt.Errorf("journal: install state.db.bak: %w", err)
	}
	if err := os.RemoveAll(old); err != nil {
		return fmt.Errorf("journal: remove swapped backup: %w", err)
	}
	return nil
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
