package journal

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
)

func sessionLockKey(sdir string) string {
	if abs, err := filepath.Abs(sdir); err == nil {
		sdir = abs
	}
	return filepath.Clean(sdir)
}

func (s *Store) noteSessionLocked(sdir string) {
	key := sessionLockKey(sdir)
	s.ownedMu.Lock()
	defer s.ownedMu.Unlock()
	if s.owned == nil {
		s.owned = make(map[string]int)
	}
	s.owned[key]++
}

func (s *Store) forgetSessionLocked(sdir string) {
	key := sessionLockKey(sdir)
	s.ownedMu.Lock()
	defer s.ownedMu.Unlock()
	if s.owned[key] <= 1 {
		delete(s.owned, key)
		return
	}
	s.owned[key]--
}

func (s *Store) sessionLockedByUs(sdir string) bool {
	key := sessionLockKey(sdir)
	s.ownedMu.Lock()
	defer s.ownedMu.Unlock()
	return s.owned[key] > 0
}

func (s *Store) attachShared() error {
	if err := s.joinMirror(); err != nil {
		return err
	}
	return s.reconcileShared()
}

func (s *Store) joinMirror() (retErr error) {
	release, err := holdLock(filepath.Join(s.dir, "repair.lock"))
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, release()) }()
	users, err := openLockFile(filepath.Join(s.dir, "users.lock"))
	if err != nil {
		return err
	}
	s.users = users
	sole, err := tryLock(users)
	if err != nil {
		return err
	}
	if sole {
		if err := s.openMirror(); err != nil {
			return err
		}
		if err := unlockFile(users); err != nil {
			return fmt.Errorf("journal: unlock %s: %w", users.Name(), err)
		}
	}
	if err := waitFileShared(users); err != nil {
		return fmt.Errorf("journal: lock %s: %w", users.Name(), err)
	}
	if sole {
		return nil
	}
	return s.attachMirror()
}

func (s *Store) attachMirror() error {
	db, err := openUsableDB(filepath.Join(s.dir, "state.db"))
	if err != nil {
		return fmt.Errorf("journal: state.db is unusable while another process uses %s: %w", s.dir, err)
	}
	s.db = db
	return s.reloadIndex()
}

func (s *Store) reconcileShared() error {
	entries, err := os.ReadDir(s.sessionDir(""))
	if err != nil {
		return fmt.Errorf("journal: read sessions: %w", err)
	}
	for _, entry := range entries {
		if !entry.IsDir() || !validSessionID(entry.Name()) {
			continue
		}
		if err := s.refreshUnowned(entry.Name()); err != nil {
			return err
		}
	}
	return nil
}

func (s *Store) refreshUnowned(sid string) (retErr error) {
	if !s.shared {
		return nil
	}
	current, err := s.isCurrent(sid)
	if err != nil || current {
		return err
	}
	sdir, err := sessionDirFor(s.dir, sid)
	if err != nil {
		return err
	}
	releaseGate, err := holdLock(filepath.Join(sdir, "repair.lock"))
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, releaseGate()) }()
	return s.reloadUnownedSession(sid, sdir)
}

func (s *Store) reloadUnownedSession(sid, sdir string) (retErr error) {
	file, err := openLockFile(filepath.Join(sdir, "lock"))
	if err != nil {
		return err
	}
	if s.sessionLockedByUs(sdir) {
		if err := file.Close(); err != nil {
			return err
		}
		return s.loadSession(sid)
	}
	owned, lockErr := tryLock(file)
	if lockErr != nil || !owned {
		return errors.Join(lockErr, file.Close())
	}
	defer func() { retErr = errors.Join(retErr, releaseLock(file)) }()
	return s.loadSession(sid)
}

func (s *Store) refreshOwned(sid string) error {
	current, err := s.isCurrent(sid)
	if err != nil || current {
		return err
	}
	return s.loadSession(sid)
}

func (s *Store) isCurrent(sid string) (bool, error) {
	s.mu.Lock()
	st := s.sessions[sid]
	s.mu.Unlock()
	info, err := os.Stat(s.journalPath(sid))
	if errors.Is(err, fs.ErrNotExist) {
		return st == nil || st.seq == 0, nil
	}
	if err != nil {
		return false, fmt.Errorf("journal: stat %s: %w", sid, err)
	}
	if st == nil || st.info == nil {
		return false, nil
	}
	same := os.SameFile(st.info, info) && st.info.Size() == info.Size() &&
		st.info.ModTime().Equal(info.ModTime())
	return same, nil
}

func (s *Store) loadSession(sid string) error {
	mirror, err := loadSessionEvents(s.db, sid)
	if err != nil {
		return fmt.Errorf("journal: load mirror index %s: %w", sid, err)
	}
	s.mu.Lock()
	old := s.sessions[sid]
	var oldFile *os.File
	if old != nil {
		oldFile = old.file
		old.file = nil
	}
	s.mu.Unlock()
	if oldFile != nil {
		_ = oldFile.Close()
	}
	result, err := s.reconcileSession(sid, mirror)
	if err != nil {
		return err
	}
	info, err := os.Stat(s.journalPath(sid))
	if err != nil {
		return fmt.Errorf("journal: stat %s: %w", sid, err)
	}
	s.mu.Lock()
	st := stateOf(result)
	st.info = info
	s.sessions[sid] = st
	s.mu.Unlock()
	return nil
}

func (st *sessionState) observe(file *os.File) {
	info, err := file.Stat()
	if err != nil {
		st.info = nil
		return
	}
	st.info = info
}

func openLockFile(path string) (*os.File, error) {
	file, err := os.OpenFile(path, os.O_RDWR|os.O_CREATE, 0o600)
	if err != nil {
		return nil, fmt.Errorf("journal: open %s: %w", path, err)
	}
	return file, nil
}

func sessionDirFor(dir, session string) (string, error) {
	return sessionDirForSync(dir, session, syncDir)
}

func sessionDirForSync(dir, session string, dirSync func(string) error) (string, error) {
	if !validSessionID(session) {
		return "", fmt.Errorf("journal: invalid session id %q", session)
	}
	sdir := filepath.Join(dir, "sessions", session)
	if err := os.MkdirAll(sdir, 0o700); err != nil {
		return "", fmt.Errorf("journal: create %s: %w", sdir, err)
	}
	if err := dirSync(filepath.Dir(sdir)); err != nil {
		return "", err
	}
	return sdir, nil
}

func tryLock(file *os.File) (bool, error) {
	err := lockFile(file)
	if err == nil {
		return true, nil
	}
	if isLockBusy(err) {
		return false, nil
	}
	return false, fmt.Errorf("journal: lock %s: %w", file.Name(), err)
}

func holdLock(path string) (func() error, error) {
	file, err := openLockFile(path)
	if err != nil {
		return nil, err
	}
	if err := waitFile(file); err != nil {
		return nil, errors.Join(fmt.Errorf("journal: lock %s: %w", file.Name(), err), file.Close())
	}
	return func() error { return releaseLock(file) }, nil
}

func releaseLock(file *os.File) error {
	if file == nil {
		return nil
	}
	return errors.Join(unlockFile(file), file.Close())
}
