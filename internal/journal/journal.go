package journal

import (
	"bufio"
	"cmp"
	"context"
	"database/sql"
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
	lock       *os.File
	users      *os.File
	shared     bool
	syncfsRoot string
	mu         sync.Mutex
	backupStop chan struct{}
	backupWg   sync.WaitGroup
	closed     bool
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
	s := &Store{dir: dir, shared: shared, sessions: map[string]*sessionState{}}
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
	load := loadIndex
	if s.shared {
		load = loadSessionRows
	}
	idx, err := load(s.db)
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

func (s *Store) Append(ctx context.Context, session string, events ...[]byte) (uint64, error) {
	if err := ctx.Err(); err != nil {
		return 0, err
	}
	if !validSessionID(session) {
		return 0, fmt.Errorf("journal: invalid session id %q", session)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	st, err := s.appendState(session)
	if err != nil {
		return 0, err
	}
	if st.damaged {
		return st.seq, fmt.Errorf("journal: session %s is damaged at seq %d", session, st.damagedSeq)
	}
	if len(events) == 0 {
		return st.seq, nil
	}
	return s.writeBatch(session, st, events)
}

func (s *Store) appendState(session string) (*sessionState, error) {
	if s.shared {
		if err := s.refreshOwned(session); err != nil {
			return nil, err
		}
	}
	st := s.sessions[session]
	if st == nil {
		st = &sessionState{hash: genesisPrev}
		s.sessions[session] = st
	}
	return st, nil
}

func (s *Store) writeBatch(session string, st *sessionState, events [][]byte) (uint64, error) {
	batch, err := buildBatch(st, events)
	if err != nil {
		return st.seq, err
	}
	file, err := s.appendFile(session)
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

func (s *Store) appendFile(session string) (*os.File, error) {
	st := s.sessions[session]
	if st.file != nil {
		return st.file, nil
	}
	dir := s.sessionDir(session)
	_, dirErr := os.Stat(dir)
	_, fileErr := os.Stat(s.journalPath(session))
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, err
	}
	file, err := os.OpenFile(s.journalPath(session), os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o600)
	if err != nil {
		return nil, fmt.Errorf("journal: open %s: %w", s.journalPath(session), err)
	}
	st.file = file
	if errors.Is(fileErr, fs.ErrNotExist) || errors.Is(dirErr, fs.ErrNotExist) {
		if syncErr := syncDir(dir); syncErr != nil {
			return nil, syncErr
		}
	}
	if errors.Is(dirErr, fs.ErrNotExist) {
		if syncErr := syncDir(filepath.Dir(dir)); syncErr != nil {
			return nil, syncErr
		}
	}
	return file, nil
}

func (s *Store) Replay(session string, fn func(seq uint64, ev []byte) error) (retErr error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.refreshUnowned(session); err != nil {
		return err
	}
	st := s.sessions[session]
	file, err := os.Open(s.journalPath(session))
	if errors.Is(err, fs.ErrNotExist) {
		if st == nil || st.seq == 0 {
			return nil
		}
		return fmt.Errorf("journal: session %s journal missing", session)
	}
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, file.Close()) }()
	cursor := replayCursor{session: session, seq: 1, prevHash: genesisPrev, fn: fn, openTail: s.shared}
	if err := replayFile(file, &cursor); err != nil {
		return err
	}
	if st != nil && st.damaged {
		return &DamageError{Session: session, Seq: st.damagedSeq}
	}
	return nil
}

type replayCursor struct {
	session  string
	seq      uint64
	prevHash string
	fn       func(uint64, []byte) error
	openTail bool
}

func replayFile(file *os.File, cursor *replayCursor) error {
	reader := bufio.NewReaderSize(file, 64*1024)
	var scratch []byte
	for {
		line, hadNL, readErr := readJournalLine(reader, &scratch)
		if line != nil && (hadNL || !cursor.openTail) {
			if err := cursor.replayLine(line, hadNL); err != nil {
				return err
			}
		}
		if readErr == nil {
			continue
		}
		if errors.Is(readErr, io.EOF) {
			return nil
		}
		return readErr
	}
}

func (c *replayCursor) replayLine(line []byte, hadNL bool) error {
	verified := verifyRecord(line, c.seq, c.prevHash)
	if verified.kind != lineOK || !hadNL {
		return fmt.Errorf("journal: %s: corrupt record at seq %d", c.session, c.seq)
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
	sum := sha256Sum(data)
	sha := hexString(sum[:])
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
	s.mu.Lock()
	defer s.mu.Unlock()
	st := s.sessions[session]
	if st == nil {
		return SessionHead{}, fmt.Errorf("journal: unknown session %s", session)
	}
	return s.headOf(session, st), nil
}

func (s *Store) headOf(id string, st *sessionState) SessionHead {
	head := SessionHead{ID: id, HeadSeq: st.seq, Status: SessionOK}
	if st.damaged {
		head.Status = SessionDamaged
		head.DamagedSeq = st.damagedSeq
	}
	return head
}

func (s *Store) Sessions() ([]SessionHead, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.shared {
		if err := s.reloadIndex(); err != nil {
			return nil, err
		}
	}
	heads := make([]SessionHead, 0, len(s.sessions)+len(s.idx.sessions))
	seen := map[string]bool{}
	for id, st := range s.sessions {
		heads = append(heads, s.headOf(id, st))
		seen[id] = true
	}
	for id, row := range s.idx.sessions {
		if seen[id] {
			continue
		}
		status := SessionStatus(row.status)
		if status == "" {
			status = SessionOK
		}
		heads = append(heads, SessionHead{ID: id, HeadSeq: row.headSeq, Status: status})
	}
	slices.SortFunc(heads, func(a, b SessionHead) int { return cmp.Compare(a.ID, b.ID) })
	return heads, nil
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
				if err := backupDB(s.db, s.dir); err != nil {
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
	hasBackup := s.backupStop != nil
	if hasBackup {
		close(s.backupStop)
	}
	db := s.db
	var result error
	for _, st := range s.sessions {
		if st.file != nil {
			result = errors.Join(result, st.file.Close())
			st.file = nil
		}
	}
	s.mu.Unlock()
	if hasBackup {
		s.backupWg.Wait()
	}
	if db != nil {
		result = errors.Join(result, backupDB(db, s.dir), db.Close())
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

func ownSession(sdir, session string) (func() error, error) {
	file, err := openLockFile(filepath.Join(sdir, "lock"))
	if err != nil {
		return nil, err
	}
	owned, err := tryLock(file)
	if err == nil && owned {
		return sync.OnceValue(func() error { return releaseLock(file) }), nil
	}
	if err == nil {
		err = fmt.Errorf("another dal process owns session %s", session)
	}
	return nil, errors.Join(err, file.Close())
}
