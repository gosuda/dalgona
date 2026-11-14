package journal

import (
	"bufio"
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"
)

const backupInterval = 30 * time.Minute

type SessionStatus string

const (
	SessionOK      SessionStatus = "ok"
	SessionDamaged SessionStatus = "damaged"
)

type SessionHead struct {
	ID         string
	HeadSeq    uint64
	Status     SessionStatus
	DamagedSeq uint64
}

type DamageError struct {
	Session string
	Seq     uint64
}

func (e *DamageError) Error() string {
	return fmt.Sprintf("journal: %s: damaged at seq %d", e.Session, e.Seq)
}

type sessionState struct {
	seq        uint64
	hash       string
	file       *os.File
	info       os.FileInfo
	damaged    bool
	damagedSeq uint64
	dirSynced  bool
}

type emitRec struct {
	seq  uint64
	hash string
	ev   []byte
}

type Store struct {
	dir        string
	db         *sql.DB
	idx        *dbIndex
	sessions   map[string]*sessionState
	slocks     map[string]*sync.Mutex
	lock       *os.File
	users      *os.File
	shared     bool
	syncfsRoot string
	dirSync    func(string) error
	mu         sync.Mutex
	backupStop chan struct{}
	backupWg   sync.WaitGroup
	closed     bool
	ownedMu    sync.Mutex
	owned      map[string]int
}

var syncfsHook = syncfsDir

func validSessionID(id string) bool {
	if id == "" || id == "." || id == ".." || len(id) > 255 {
		return false
	}
	return !strings.ContainsAny(id, "/\\\x00")
}

func (s *Store) sessionDir(id string) string {
	return filepath.Join(s.dir, "sessions", id)
}

func (s *Store) journalPath(id string) string {
	return filepath.Join(s.sessionDir(id), "journal.jsonl")
}

func (s *Store) quarantineDir() string {
	return filepath.Join(s.dir, "quarantine")
}

func (s *Store) blobsDir() string {
	return filepath.Join(s.dir, "blobs")
}

func Open(dir string) (*Store, error) {
	return open(dir, false)
}

func OpenShared(dir string) (*Store, error) {
	return open(dir, true)
}

func open(dir string, shared bool) (_ *Store, retErr error) {
	if dir == "" {
		return nil, errors.New("journal: empty home dir")
	}
	s := &Store{
		dir:      dir,
		shared:   shared,
		sessions: map[string]*sessionState{},
		slocks:   map[string]*sync.Mutex{},
		owned:    map[string]int{},
		dirSync:  syncDir,
	}
	defer func() {
		if retErr != nil {
			retErr = errors.Join(retErr, s.closeLocked())
		}
	}()
	for _, sub := range []string{s.sessionDir(""), s.blobsDir(), s.quarantineDir()} {
		if err := os.MkdirAll(sub, 0o700); err != nil {
			return nil, fmt.Errorf("journal: create %s: %w", sub, err)
		}
	}
	s.syncfsRoot = os.Getenv("DALGONA_SYNCFS")
	if err := s.attach(); err != nil {
		return nil, err
	}
	s.startBackup()
	return s, nil
}

func (s *Store) attach() error {
	if s.shared {
		return s.attachShared()
	}
	lock, err := acquireHomeLock(s.dir)
	if err != nil {
		return err
	}
	s.lock = lock
	if err := s.openMirror(); err != nil {
		return err
	}
	return s.reconcileStore()
}

func acquireHomeLock(dir string) (*os.File, error) {
	lock, err := openLockFile(filepath.Join(dir, "daemon.lock"))
	if err != nil {
		return nil, err
	}
	owned, err := tryLock(lock)
	if err == nil && owned {
		return lock, nil
	}
	if err == nil {
		err = fmt.Errorf("another dalgona process owns %s", dir)
	}
	return nil, errors.Join(err, lock.Close())
}

func (s *Store) openMirror() error {
	path := filepath.Join(s.dir, "state.db")
	db, openErr := openUsableDB(path)
	if openErr == nil {
		s.db = db
		return s.reloadIndex()
	}
	if err := quarantineDB(s.dir, s.quarantineDir()); err != nil {
		return errors.Join(openErr, err)
	}
	db, backupErr := recoverBackup(s.dir)
	if backupErr == nil && db != nil {
		s.db = db
		return s.reloadIndex()
	}
	if backupErr != nil {
		slog.Warn("journal: state.db.bak could not be restored", "error", backupErr)
	}
	if err := quarantineDB(s.dir, s.quarantineDir()); err != nil {
		return errors.Join(openErr, backupErr, err)
	}
	db, err := openDB(path)
	if err != nil {
		return err
	}
	s.db = db
	return s.reloadIndex()
}

func (s *Store) reconcileStore() error {
	needsRebuild, err := s.reconcileAll()
	if err != nil || !needsRebuild {
		return err
	}
	rebuildErr := s.rebuildMirror()
	if rebuildErr != nil {
		return rebuildErr
	}
	s.sessions = map[string]*sessionState{}
	needsRebuild, err = s.reconcileAll()
	if needsRebuild {
		return errors.New("journal: rebuilt mirror still disagrees with journals")
	}
	return err
}

func (s *Store) rebuildMirror() error {
	if s.db != nil {
		if err := s.db.Close(); err != nil {
			return err
		}
		s.db = nil
	}
	if err := quarantineDB(s.dir, s.quarantineDir()); err != nil {
		return fmt.Errorf("journal: quarantine state.db: %w", err)
	}
	db, err := openDB(filepath.Join(s.dir, "state.db"))
	if err != nil {
		return err
	}
	s.db = db
	s.idx = &dbIndex{events: map[string]*sessIndex{}, sessions: map[string]sessionRow{}}
	return nil
}

func (s *Store) reloadIndex() error {
	idx, err := loadSessionRows(s.db)
	if err != nil {
		return fmt.Errorf("journal: load mirror index: %w", err)
	}
	s.idx = idx
	return nil
}

type appendBatch struct {
	buf  []byte
	recs []emitRec
	seq  uint64
	hash string
}

func buildBatch(st *sessionState, events [][]byte) (appendBatch, error) {
	batch := appendBatch{
		recs: make([]emitRec, 0, len(events)),
		seq:  st.seq,
		hash: st.hash,
	}
	for _, ev := range events {
		if err := validEventJSON(ev); err != nil {
			return appendBatch{}, err
		}
		batch.seq++
		hash := hexOfSum(hashRecord(batch.hash, batch.seq, ev))
		lineStart := len(batch.buf)
		batch.buf = encodeLine(batch.buf, batch.seq, batch.hash, hash, ev)
		if len(batch.buf)-lineStart > maxLine {
			return appendBatch{}, errors.New("journal: event exceeds 16 MiB line limit")
		}
		batch.recs = append(batch.recs, emitRec{seq: batch.seq, hash: hash, ev: ev})
		batch.hash = hash
	}
	return batch, nil
}

func (s *Store) lockFor(session string) *sync.Mutex {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.slocks == nil {
		s.slocks = map[string]*sync.Mutex{}
	}
	l, ok := s.slocks[session]
	if !ok {
		l = &sync.Mutex{}
		s.slocks[session] = l
	}
	return l
}

func (s *Store) Append(ctx context.Context, session string, events ...[]byte) (uint64, error) {
	if err := ctx.Err(); err != nil {
		return 0, err
	}
	if !validSessionID(session) {
		return 0, fmt.Errorf("journal: invalid session id %q", session)
	}
	l := s.lockFor(session)
	l.Lock()
	defer l.Unlock()
	if s.shared {
		if err := s.refreshOwned(session); err != nil {
			return 0, err
		}
	}
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return 0, errors.New("journal: store is closed")
	}
	st := s.sessions[session]
	if st == nil {
		st = &sessionState{hash: genesisPrev}
		s.sessions[session] = st
	}
	s.mu.Unlock()
	if st.damaged {
		return st.seq, fmt.Errorf("journal: session %s is damaged at seq %d", session, st.damagedSeq)
	}
	if len(events) == 0 {
		return st.seq, nil
	}
	return s.writeBatch(session, st, events)
}

func (s *Store) writeBatch(session string, st *sessionState, events [][]byte) (uint64, error) {
	batch, err := buildBatch(st, events)
	if err != nil {
		return st.seq, err
	}
	file, err := s.appendFile(session, st)
	if err != nil {
		return st.seq, err
	}
	if err := writeAppend(file, batch.buf, session); err != nil {
		st.damaged = true
		st.damagedSeq = st.seq + 1
		return st.seq, err
	}
	st.seq, st.hash = batch.seq, batch.hash
	if s.shared {
		st.observe(file)
	}
	syncErr := s.syncfs()
	mirrorErr := mirrorAppend(s.db, session, batch.recs, batch.seq)
	return batch.seq, errors.Join(syncErr, mirrorErr)
}

func writeAppend(file *os.File, data []byte, session string) error {
	n, err := file.Write(data)
	if err != nil {
		return fmt.Errorf("journal: append %s: %w", session, err)
	}
	if n != len(data) {
		return fmt.Errorf("journal: append %s: short write %d of %d", session, n, len(data))
	}
	if err := file.Sync(); err != nil {
		return fmt.Errorf("journal: fsync %s: %w", session, err)
	}
	return nil
}

func (s *Store) syncfs() error {
	if s.syncfsRoot == "" {
		return nil
	}
	if err := syncfsHook(s.syncfsRoot); err != nil {
		return fmt.Errorf("journal: syncfs %s: %w", s.syncfsRoot, err)
	}
	return nil
}

func (s *Store) appendFile(session string, st *sessionState) (*os.File, error) {
	if st.file != nil {
		return st.file, nil
	}
	dir := s.sessionDir(session)
	if err := os.MkdirAll(dir, 0o700); err != nil { //nolint:gosec // session directory path is validated
		return nil, err
	}
	file, err := os.OpenFile(s.journalPath(session), os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o600) //nolint:gosec // session journal path is validated
	if err != nil {
		return nil, fmt.Errorf("journal: open %s: %w", s.journalPath(session), err)
	}
	if !st.dirSynced {
		if syncErr := s.dirSync(dir); syncErr != nil {
			_ = file.Close()
			return nil, syncErr
		}
		if syncErr := s.dirSync(filepath.Dir(dir)); syncErr != nil {
			_ = file.Close()
			return nil, syncErr
		}
		st.dirSynced = true
	}
	st.file = file
	return file, nil
}

func (s *Store) Replay(session string, fn func(seq uint64, ev []byte) error) error {
	snap, err := s.beginReplay(session)
	if err != nil {
		return err
	}
	if snap.file == nil {
		return nil
	}
	cursor := replayCursor{session: session, seq: 1, prevHash: genesisPrev, fn: fn, openTail: s.shared}
	replayErr := replayFile(snap.file, snap.limit, &cursor)
	if closeErr := snap.file.Close(); closeErr != nil || replayErr != nil {
		return errors.Join(replayErr, closeErr)
	}
	if snap.damaged {
		return &DamageError{Session: session, Seq: snap.damagedSeq}
	}
	return nil
}

type replaySnapshot struct {
	file       *os.File
	limit      int64
	damaged    bool
	damagedSeq uint64
}

func (s *Store) beginReplay(session string) (replaySnapshot, error) {
	l := s.lockFor(session)
	l.Lock()
	defer l.Unlock()
	if err := s.refreshUnowned(session); err != nil {
		return replaySnapshot{}, err
	}
	s.mu.Lock()
	st := s.sessions[session]
	var seq uint64
	var damaged bool
	var damagedSeq uint64
	if st != nil {
		seq, damaged, damagedSeq = st.seq, st.damaged, st.damagedSeq
	}
	s.mu.Unlock()
	file, err := os.Open(s.journalPath(session))
	if errors.Is(err, fs.ErrNotExist) {
		if st == nil || seq == 0 {
			return replaySnapshot{}, nil
		}
		return replaySnapshot{}, fmt.Errorf("journal: session %s journal missing", session)
	}
	if err != nil {
		return replaySnapshot{}, err
	}
	info, err := file.Stat()
	if err != nil {
		return replaySnapshot{}, errors.Join(err, file.Close())
	}
	return replaySnapshot{file: file, limit: info.Size(), damaged: damaged, damagedSeq: damagedSeq}, nil
}

type replayCursor struct {
	session  string
	seq      uint64
	prevHash string
	fn       func(uint64, []byte) error
	openTail bool
}

type replayScanner struct {
	file    *os.File
	limit   int64
	cursor  *replayCursor
	reader  *bufio.Reader
	scratch []byte
}

func replayFile(file *os.File, limit int64, cursor *replayCursor) error {
	scanner := &replayScanner{
		file:   file,
		limit:  limit,
		cursor: cursor,
		reader: bufio.NewReaderSize(io.LimitReader(file, limit), 64*1024),
	}
	for {
		line, hadNL, readErr := readJournalLine(scanner.reader, &scanner.scratch)
		stop, err := scanner.chunk(line, hadNL, readErr)
		if err != nil {
			return err
		}
		if stop {
			return nil
		}
	}
}

func (s *replayScanner) chunk(line []byte, hadNL bool, readErr error) (bool, error) {
	if line != nil && (hadNL || !s.cursor.openTail) {
		if s.cutShort(hadNL, readErr) {
			return true, nil
		}
		if err := s.cursor.replayLine(line, hadNL); err != nil {
			return true, err
		}
	}
	if readErr == nil {
		return false, nil
	}
	if errors.Is(readErr, io.EOF) {
		return true, nil
	}
	return true, readErr
}

func (s *replayScanner) cutShort(hadNL bool, readErr error) bool {
	if hadNL || s.cursor.openTail || !errors.Is(readErr, io.EOF) {
		return false
	}
	grown, err := fileGrownBeyond(s.file, s.limit)
	return err == nil && grown
}

func fileGrownBeyond(file *os.File, limit int64) (bool, error) {
	info, err := file.Stat()
	if err != nil {
		return false, err
	}
	return info.Size() > limit, nil
}

func (c *replayCursor) replayLine(line []byte, hadNL bool) error {
	verified := verifyRecord(line, c.seq, c.prevHash)
	if verified.kind != lineOK || !hadNL {
		return &DamageError{Session: c.session, Seq: c.seq}
	}
	if err := c.fn(verified.seq, verified.ev); err != nil {
		return err
	}
	c.prevHash = verified.hash
	c.seq++
	return nil
}

func readJournalLine(reader *bufio.Reader, scratch *[]byte) ([]byte, bool, error) {
	acc := (*scratch)[:0]
	for {
		frag, err := reader.ReadSlice('\n')
		acc = append(acc, frag...)
		if len(acc) > maxLine {
			return nil, false, errors.New("journal: line exceeds 16 MiB")
		}
		if !errors.Is(err, bufio.ErrBufferFull) {
			*scratch = acc
			return finishJournalLine(acc, err)
		}
	}
}

func finishJournalLine(line []byte, err error) ([]byte, bool, error) {
	hadNL := len(line) > 0 && line[len(line)-1] == '\n'
	if hadNL {
		line = line[:len(line)-1]
	}
	if err == nil {
		return line, hadNL, nil
	}
	if errors.Is(err, io.EOF) && len(line) > 0 {
		return line, hadNL, io.EOF
	}
	if errors.Is(err, io.EOF) {
		return nil, false, io.EOF
	}
	return nil, false, err
}

func (s *Store) PutBlob(data []byte) (string, error) {
	sum := sha256.Sum256(data)
	sha := hex.EncodeToString(sum[:])
	path := filepath.Join(s.blobsDir(), sha)
	if _, err := os.Stat(path); err == nil {
		return sha, nil
	} else if !errors.Is(err, fs.ErrNotExist) {
		return "", err
	}
	if err := writeBlob(path, data); err != nil {
		return "", err
	}
	return sha, nil
}

func writeBlob(path string, data []byte) (retErr error) {
	tmp, err := os.CreateTemp(filepath.Dir(path), ".tmp-*")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	closed := false
	defer func() {
		if !closed {
			retErr = errors.Join(retErr, tmp.Close())
		}
		if retErr != nil {
			retErr = errors.Join(retErr, os.Remove(tmpName))
		}
	}()
	if err := writeAppend(tmp, data, tmpName); err != nil {
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	closed = true
	if err := os.Rename(tmpName, path); err != nil {
		return err
	}
	return syncDir(filepath.Dir(path))
}

func (s *Store) Blob(sha string) ([]byte, error) {
	if len(sha) != hexHashLen || sha != strings.ToLower(sha) {
		return nil, fmt.Errorf("journal: bad blob id %q", sha)
	}
	if _, err := hexDecode(sha); err != nil {
		return nil, fmt.Errorf("journal: bad blob id %q", sha)
	}
	data, err := os.ReadFile(filepath.Join(s.blobsDir(), sha))
	if errors.Is(err, fs.ErrNotExist) {
		return nil, fmt.Errorf("journal: blob %s not found", sha)
	}
	if err != nil {
		return nil, err
	}
	return data, nil
}

func (s *Store) Status(session string) (SessionHead, error) {
	l := s.lockFor(session)
	l.Lock()
	defer l.Unlock()
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.shared {
		if err := s.reloadIndex(); err != nil {
			return SessionHead{}, err
		}
	}
	st := s.sessions[session]
	if st == nil {
		if row, ok := s.idx.sessions[session]; ok {
			return mirrorRowHead(session, row), nil
		}
		return SessionHead{}, fmt.Errorf("journal: unknown session %s", session)
	}
	return s.freshHead(session, st), nil
}

func (s *Store) headOf(id string, st *sessionState) SessionHead {
	head := SessionHead{ID: id, HeadSeq: st.seq, Status: SessionOK}
	if st.damaged {
		head.Status = SessionDamaged
		head.DamagedSeq = st.damagedSeq
	}
	return head
}

func (s *Store) sessionIDs() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.sessionIDsLocked()
}

func (s *Store) sessionIDsLocked() []string {
	seen := map[string]bool{}
	ids := make([]string, 0, len(s.sessions)+len(s.idx.sessions))
	for id := range s.sessions {
		if !seen[id] {
			seen[id] = true
			ids = append(ids, id)
		}
	}
	for id := range s.idx.sessions {
		if !seen[id] {
			seen[id] = true
			ids = append(ids, id)
		}
	}
	slices.Sort(ids)
	return ids
}

func (s *Store) lockSessions(ids []string) func() {
	locks := make([]*sync.Mutex, 0, len(ids))
	for _, id := range ids {
		locks = append(locks, s.lockFor(id))
	}
	for _, l := range locks {
		l.Lock()
	}
	return func() {
		for _, l := range locks {
			l.Unlock()
		}
	}
}

func (s *Store) Sessions() ([]SessionHead, error) {
	s.mu.Lock()
	if s.shared {
		if err := s.reloadIndex(); err != nil {
			s.mu.Unlock()
			return nil, err
		}
	}
	ids := s.sessionIDsLocked()
	s.mu.Unlock()
	unlock := s.lockSessions(ids)
	defer unlock()
	s.mu.Lock()
	defer s.mu.Unlock()
	heads := make([]SessionHead, 0, len(ids))
	for _, id := range ids {
		if st := s.sessions[id]; st != nil {
			heads = append(heads, s.freshHead(id, st))
			continue
		}
		if row, ok := s.idx.sessions[id]; ok {
			heads = append(heads, mirrorRowHead(id, row))
		}
	}
	return heads, nil
}

func (s *Store) freshHead(id string, st *sessionState) SessionHead {
	if s.shared {
		if row, ok := s.idx.sessions[id]; ok && !st.damaged {
			return mirrorRowHead(id, row)
		}
	}
	return s.headOf(id, st)
}

func mirrorRowHead(id string, row sessionRow) SessionHead {
	status := SessionStatus(row.status)
	if status != SessionOK && status != SessionDamaged {
		status = SessionDamaged
	}
	return SessionHead{ID: id, HeadSeq: row.headSeq, Status: status}
}

func (s *Store) startBackup() {
	s.backupStop = make(chan struct{})
	s.backupWg.Go(func() {
		ticker := time.NewTicker(backupInterval)
		defer ticker.Stop()
		for {
			select {
			case <-s.backupStop:
				return
			case <-ticker.C:
				if err := s.backupDB(); err != nil {
					slog.Warn("journal: backup failed", "error", err)
				}
			}
		}
	})
}

func (s *Store) Close() error {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return nil
	}
	s.closed = true
	stopBackup := s.backupStop != nil
	if stopBackup {
		close(s.backupStop)
	}
	s.mu.Unlock()
	if stopBackup {
		s.backupWg.Wait()
	}
	s.mu.Lock()
	ids := s.sessionIDsLocked()
	s.mu.Unlock()
	unlock := s.lockSessions(ids)
	defer unlock()
	s.mu.Lock()
	db := s.db
	dir := s.dir
	dirSync := s.dirSync
	var fileErr error
	for _, st := range s.sessions {
		if st.file != nil {
			fileErr = errors.Join(fileErr, st.file.Close())
			st.file = nil
		}
	}
	s.mu.Unlock()
	result := fileErr
	if db != nil {
		result = errors.Join(result, backupDBFiles(db, dir, dirSync), db.Close())
	}
	return errors.Join(result, releaseLock(s.users), releaseLock(s.lock))
}

func (s *Store) closeLocked() error {
	var result error
	if s.db != nil {
		result = errors.Join(result, s.db.Close())
	}
	return errors.Join(result, releaseLock(s.users), releaseLock(s.lock))
}

func LockSession(dir, session string) (func() error, error) {
	sdir, err := sessionDirFor(dir, session)
	if err != nil {
		return nil, err
	}
	releaseGate, err := holdLock(filepath.Join(sdir, "repair.lock"))
	if err != nil {
		return nil, err
	}
	unlock, err := ownSession(sdir, session)
	gateErr := releaseGate()
	if gateErr != nil && unlock != nil {
		gateErr = errors.Join(gateErr, unlock())
		unlock = nil
	}
	return unlock, errors.Join(err, gateErr)
}

func (s *Store) LockSession(session string) (func() error, error) {
	sdir, err := sessionDirFor(s.dir, session)
	if err != nil {
		return nil, err
	}
	releaseGate, err := holdLock(filepath.Join(sdir, "repair.lock"))
	if err != nil {
		return nil, err
	}
	unlock, err := s.ownSession(sdir, session)
	gateErr := releaseGate()
	if gateErr != nil && unlock != nil {
		gateErr = errors.Join(gateErr, unlock())
		unlock = nil
	}
	return unlock, errors.Join(err, gateErr)
}

func ownSession(sdir, session string) (func() error, error) {
	file, err := openLockFile(filepath.Join(sdir, "lock"))
	if err != nil {
		return nil, err
	}
	owned, err := tryLock(file)
	if err == nil && owned {
		return sync.OnceValue(func() error {
			return releaseLock(file)
		}), nil
	}
	if err == nil {
		err = fmt.Errorf("another dal process owns session %s", session)
	}
	return nil, errors.Join(err, file.Close())
}

func (s *Store) ownSession(sdir, session string) (func() error, error) {
	file, err := openLockFile(filepath.Join(sdir, "lock"))
	if err != nil {
		return nil, err
	}
	owned, err := tryLock(file)
	if err == nil && owned {
		s.noteSessionLocked(sdir)
		released := false
		return func() error {
			if released {
				return nil
			}
			releaseGate, gateErr := holdRepairGate(sdir)
			if gateErr != nil {
				return gateErr
			}
			lockErr := releaseLock(file)
			if lockErr != nil {
				return errors.Join(lockErr, releaseGate())
			}
			s.forgetSessionLocked(sdir)
			released = true
			return releaseGate()
		}, nil
	}
	if err == nil {
		err = fmt.Errorf("another dal process owns session %s", session)
	}
	return nil, errors.Join(err, file.Close())
}

func holdRepairGate(sdir string) (func() error, error) {
	var releaseGate func() error
	var gateErr error
	for range 3 {
		releaseGate, gateErr = holdLock(filepath.Join(sdir, "repair.lock"))
		if gateErr == nil {
			return releaseGate, nil
		}
		time.Sleep(time.Millisecond)
	}
	return nil, gateErr
}
