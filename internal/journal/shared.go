package journal

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
)

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
	file, err := openLockFile(filepath.Join(sdir, "lock"))
	if err != nil {
		return err
	}
	owned, err := tryLock(file)
	if err != nil || !owned {
		return errors.Join(err, file.Close())
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
	st := s.sessions[sid]
	info, err := os.Stat(s.journalPath(sid))
	if errors.Is(err, fs.ErrNotExist) {
		return st == nil || st.seq == 0, nil
	}
	if err != nil {
		return false, fmt.Errorf("journal: stat %s: %w", sid, err)
	}
	return st != nil && st.info != nil && os.SameFile(st.info, info) && st.info.Size() == info.Size(), nil
}

func (s *Store) loadSession(sid string) error {
	mirror, err := loadSessionEvents(s.db, sid)
	if err != nil {
		return fmt.Errorf("journal: load mirror index %s: %w", sid, err)
	}
	s.idx.events[sid] = mirror
	defer delete(s.idx.events, sid)
	result, err := s.reconcileSession(sid)
	if err != nil {
		return err
	}
	info, err := os.Stat(s.journalPath(sid))
	if err != nil {
		return fmt.Errorf("journal: stat %s: %w", sid, err)
	}
	var closeErr error
	if old := s.sessions[sid]; old != nil && old.file != nil {
		closeErr = old.file.Close()
	}
	st := stateOf(result)
	st.info = info
	s.sessions[sid] = st
	return closeErr
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
