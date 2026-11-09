package journal

import (
	"database/sql"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

const sessionsColumns = `(id TEXT PRIMARY KEY, parent TEXT, cwd TEXT, title TEXT, status TEXT NOT NULL CHECK(status IN ('ok','damaged')), head_seq INTEGER NOT NULL CHECK(head_seq >= 0), updated_at TEXT)`

const schema = `
CREATE TABLE IF NOT EXISTS events(
	session TEXT NOT NULL,
	seq INTEGER NOT NULL,
	hash TEXT NOT NULL,
	ev BLOB NOT NULL,
	PRIMARY KEY(session, seq)
);
CREATE TABLE IF NOT EXISTS sessions` + sessionsColumns + `;
`

func dbDSN(path string) string {
	esc := strings.NewReplacer("%", "%25", "?", "%3F", "#", "%23", " ", "%20").Replace(path)
	return "file:" + esc + "?_pragma=journal_mode(WAL)" +
		"&_pragma=synchronous(FULL)" +
		"&_pragma=foreign_keys(1)" +
		"&_pragma=busy_timeout(5000)" +
		"&_txlock=immediate"
}

func openDB(path string) (db *sql.DB, err error) {
	db, err = sql.Open("sqlite", dbDSN(path))
	if err != nil {
		return nil, fmt.Errorf("journal: open state.db: %w", err)
	}
	db.SetMaxOpenConns(1)
	if _, err = db.Exec(schema); err != nil {
		return nil, errors.Join(fmt.Errorf("journal: create schema: %w", err), db.Close())
	}
	if err = migrateDB(db); err != nil {
		return nil, errors.Join(fmt.Errorf("journal: migrate state.db: %w", err), db.Close())
	}
	return db, nil
}

const schemaVersion = 2

func migrateDB(db *sql.DB) error {
	version, err := schemaVersionOf(db)
	if err != nil {
		return err
	}
	if version > schemaVersion {
		return fmt.Errorf("journal: state.db version %d exceeds supported %d", version, schemaVersion)
	}
	for version < schemaVersion {
		if err := applyMigration(db, version+1); err != nil {
			return err
		}
		version++
	}
	return nil
}

func schemaVersionOf(db *sql.DB) (int, error) {
	var version int
	if err := db.QueryRow("PRAGMA user_version").Scan(&version); err != nil {
		return 0, err
	}
	return version, nil
}

func setUserVersion(db *sql.DB, version int) error {
	_, err := db.Exec(fmt.Sprintf("PRAGMA user_version = %d", version))
	return err
}

func applyMigration(db *sql.DB, target int) error {
	switch target {
	case 1:
		return migrateToV1(db)
	case 2:
		return migrateToV2(db)
	default:
		return fmt.Errorf("journal: unknown schema version %d", target)
	}
}

func migrateToV1(db *sql.DB) error {
	return setUserVersion(db, 1)
}

func migrateToV2(db *sql.DB) error {
	if err := withTx(db, func(tx *sql.Tx) error {
		if _, err := tx.Exec(`UPDATE sessions SET status = 'damaged' WHERE status IS NULL OR status NOT IN ('ok','damaged')`); err != nil {
			return err
		}
		if _, err := tx.Exec(`UPDATE sessions SET head_seq = 0 WHERE head_seq IS NULL OR head_seq < 0`); err != nil {
			return err
		}
		for _, stmt := range sessionsV2Rebuild() {
			if _, err := tx.Exec(stmt); err != nil {
				return err
			}
		}
		return nil
	}); err != nil {
		return err
	}
	return setUserVersion(db, 2)
}

func sessionsV2Rebuild() []string {
	return []string{
		"CREATE TABLE sessions_new" + sessionsColumns,
		"INSERT INTO sessions_new SELECT id, parent, cwd, title, status, head_seq, updated_at FROM sessions",
		"DROP TABLE sessions",
		"ALTER TABLE sessions_new RENAME TO sessions",
	}
}

func openUsableDB(path string) (*sql.DB, error) {
	db, err := openDB(path)
	if err != nil {
		return nil, err
	}
	ok, checkErr := quickCheckOK(db)
	if checkErr == nil && ok {
		return db, nil
	}
	if checkErr == nil {
		checkErr = errors.New("quick_check reported corruption")
	}
	return nil, errors.Join(fmt.Errorf("journal: quick_check: %w", checkErr), db.Close())
}

func recoverBackup(dir string) (*sql.DB, error) {
	source := filepath.Join(dir, "state.db.bak")
	if _, err := os.Stat(source); errors.Is(err, os.ErrNotExist) {
		return nil, nil
	} else if err != nil {
		return nil, err
	}
	tmp := filepath.Join(dir, "state.db.recover.tmp")
	if err := copyMirrorFile(source, tmp); err != nil {
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

func quickCheckOK(db *sql.DB) (ok bool, retErr error) {
	rows, err := db.Query("PRAGMA quick_check")
	if err != nil {
		return false, err
	}
	defer func() { retErr = errors.Join(retErr, rows.Close()) }()
	for rows.Next() {
		var result string
		if err := rows.Scan(&result); err != nil {
			return false, err
		}
		if result != "ok" {
			return false, nil
		}
	}
	return true, rows.Err()
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

func loadSessionRows(db *sql.DB) (*dbIndex, error) {
	idx := &dbIndex{
		events:   map[string]*sessIndex{},
		sessions: map[string]sessionRow{},
	}
	if err := loadSessionIndex(db, idx.sessions); err != nil {
		return nil, err
	}
	return idx, nil
}

func loadSessionEvents(db *sql.DB, sid string) (_ *sessIndex, retErr error) {
	rows, err := db.Query("SELECT seq, hash FROM events WHERE session = ?", sid)
	if err != nil {
		return nil, err
	}
	defer func() { retErr = errors.Join(retErr, rows.Close()) }()
	events := map[string]*sessIndex{}
	for rows.Next() {
		var seq uint64
		var hash string
		if err := rows.Scan(&seq, &hash); err != nil {
			return nil, err
		}
		indexEvent(events, sid, seq, hash)
	}
	return events[sid], rows.Err()
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

func loadSessionIndex(db *sql.DB, sessions map[string]sessionRow) (retErr error) {
	rows, err := db.Query("SELECT id, status, head_seq FROM sessions")
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, rows.Close()) }()
	for rows.Next() {
		var id string
		var status sql.NullString
		var head sql.NullInt64
		if err := rows.Scan(&id, &status, &head); err != nil {
			return err
		}
		sessions[id] = coerceSessionRow(status, head)
	}
	return rows.Err()
}

func coerceSessionRow(status sql.NullString, head sql.NullInt64) sessionRow {
	row := sessionRow{status: status.String, headSeq: uint64(max(head.Int64, 0))}
	if !status.Valid || (row.status != string(SessionOK) && row.status != string(SessionDamaged)) {
		row.status = string(SessionDamaged)
	}
	if !head.Valid || head.Int64 < 0 {
		row.headSeq = 0
	}
	return row
}

type fetchResult struct {
	hash string
	ev   []byte
	ok   bool
}

func fetchRow(db *sql.DB, sid string, seq uint64) (fetchResult, error) {
	row := db.QueryRow("SELECT hash, ev FROM events WHERE session = ? AND seq = ?", sid, seq)
	var result fetchResult
	err := row.Scan(&result.hash, &result.ev)
	if errors.Is(err, sql.ErrNoRows) {
		return fetchResult{}, nil
	}
	if err != nil {
		return fetchResult{}, err
	}
	result.ok = true
	return result, nil
}

func withTx(db *sql.DB, fn func(*sql.Tx) error) (retErr error) {
	tx, err := db.Begin()
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, rollbackTx(tx)) }()
	if err := fn(tx); err != nil {
		return err
	}
	return tx.Commit()
}

func rollbackTx(tx *sql.Tx) error {
	err := tx.Rollback()
	if errors.Is(err, sql.ErrTxDone) {
		return nil
	}
	return err
}

func insertEventsTx(tx *sql.Tx, sid string, recs []emitRec) (retErr error) {
	if len(recs) == 0 {
		return nil
	}
	stmt, err := tx.Prepare("INSERT INTO events(session, seq, hash, ev) VALUES(?, ?, ?, ?)")
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, stmt.Close()) }()
	for _, rec := range recs {
		if _, err := stmt.Exec(sid, rec.seq, rec.hash, rec.ev); err != nil {
			return err
		}
	}
	return nil
}

func mirrorAppend(db *sql.DB, sid string, recs []emitRec, headSeq uint64) error {
	return withTx(db, func(tx *sql.Tx) error {
		if err := insertEventsTx(tx, sid, recs); err != nil {
			return err
		}
		return upsertSessionTx(tx, sid, string(SessionOK), headSeq)
	})
}

func replaceSession(db *sql.DB, sid string, recs []emitRec, head SessionHead) error {
	return withTx(db, func(tx *sql.Tx) error {
		if _, err := tx.Exec("DELETE FROM events WHERE session = ?", sid); err != nil {
			return err
		}
		if err := insertEventsTx(tx, sid, recs); err != nil {
			return err
		}
		return upsertSessionTx(tx, sid, string(head.Status), head.HeadSeq)
	})
}

func upsertSession(db *sql.DB, sid string, status string, headSeq uint64) error {
	_, err := db.Exec(upsertSessionSQL, sid, status, headSeq, time.Now().UTC().Format(time.RFC3339))
	return err
}

func upsertSessionTx(tx *sql.Tx, sid string, status string, headSeq uint64) error {
	_, err := tx.Exec(upsertSessionSQL, sid, status, headSeq, time.Now().UTC().Format(time.RFC3339))
	return err
}

const upsertSessionSQL = `INSERT INTO sessions(id, status, head_seq, updated_at)
	VALUES(?, ?, ?, ?)
	ON CONFLICT(id) DO UPDATE SET
		head_seq = excluded.head_seq,
		updated_at = excluded.updated_at,
		status = CASE WHEN sessions.status = 'damaged' THEN 'damaged' ELSE excluded.status END`

func backupDB(db *sql.DB, dir string) (retErr error) {
	release, err := holdLock(filepath.Join(dir, "repair.lock"))
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, release()) }()
	tmp := filepath.Join(dir, "state.db.bak.tmp")
	final := filepath.Join(dir, "state.db.bak")
	if err := os.Remove(tmp); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("journal: remove stale backup: %w", err)
	}
	if _, err := db.Exec("VACUUM INTO ?", tmp); err != nil {
		return fmt.Errorf("journal: backup state.db: %w", err)
	}
	if err := os.Rename(tmp, final); err != nil {
		return fmt.Errorf("journal: install state.db.bak: %w", err)
	}
	return nil
}

func quarantineDB(dir, quarantine string) error {
	stem, err := quarantineStem(quarantine)
	if err != nil {
		return err
	}
	var result error
	for _, suffix := range []string{"", "-wal", "-shm"} {
		src := filepath.Join(dir, "state.db"+suffix)
		if _, err := os.Stat(src); err != nil {
			continue
		}
		if err := os.Rename(src, stem+suffix); err != nil {
			result = errors.Join(result, err)
		}
	}
	return result
}

func quarantineStem(quarantine string) (string, error) {
	base := fmt.Sprintf("state-%d-%d.db", time.Now().UnixNano(), os.Getpid())
	for attempt := range 100 {
		name := base
		if attempt > 0 {
			name = fmt.Sprintf("%s-%d", base, attempt)
		}
		if free, err := quarantineFree(quarantine, name); err != nil || !free {
			if err != nil {
				return "", err
			}
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
