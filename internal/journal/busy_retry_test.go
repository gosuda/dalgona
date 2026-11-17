package journal

import (
	"database/sql"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestWithTxRetriesAfterBusy(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.db")
	dsn := strings.Replace(dbDSN(path), "busy_timeout(5000)", "busy_timeout(50)", 1) + "&cache=private"
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	db.SetMaxOpenConns(1)
	if _, setupErr := db.Exec(schema); setupErr != nil {
		t.Fatal(setupErr)
	}
	blocker, err := sql.Open("sqlite", dsn)
	if err != nil {
		t.Fatal(err)
	}
	defer blocker.Close()
	if _, beginErr := blocker.Exec("BEGIN EXCLUSIVE"); beginErr != nil {
		t.Fatal(beginErr)
	}

	releaseDone := make(chan struct{})
	go func() {
		defer close(releaseDone)
		time.Sleep(200 * time.Millisecond)
		_, _ = blocker.Exec("COMMIT")
	}()

	start := time.Now()
	err = withTx(db, func(tx *sql.Tx) error {
		_, insertErr := tx.Exec("INSERT INTO sessions(id, status, head_seq, updated_at) VALUES('s','ok',0,'x')")
		return insertErr
	})
	<-releaseDone
	if err != nil {
		t.Fatalf("withTx: %v", err)
	}
	if elapsed := time.Since(start); elapsed < 50*time.Millisecond {
		t.Fatalf("withTx returned after %v without waiting through the busy timeout", elapsed)
	}
}

func TestWithTxReportsBusyAfterRetries(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.db")
	dsn := strings.Replace(dbDSN(path), "busy_timeout(5000)", "busy_timeout(50)", 1) + "&cache=private"
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	db.SetMaxOpenConns(1)
	if _, setupErr := db.Exec(schema); setupErr != nil {
		t.Fatal(setupErr)
	}
	blocker, err := sql.Open("sqlite", dsn)
	if err != nil {
		t.Fatal(err)
	}
	defer blocker.Close()
	if _, beginErr := blocker.Exec("BEGIN EXCLUSIVE"); beginErr != nil {
		t.Fatal(beginErr)
	}
	defer func() {
		_, _ = blocker.Exec("COMMIT")
	}()

	err = withTx(db, func(tx *sql.Tx) error {
		_, insertErr := tx.Exec("INSERT INTO sessions(id, status, head_seq, updated_at) VALUES('s','ok',0,'x')")
		return insertErr
	})
	if err == nil || !isBusyError(err) {
		t.Fatalf("withTx = %v, want SQLITE_BUSY", err)
	}
}

func TestWithTxDoesNotRetryOtherErrors(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.db")
	db, err := sql.Open("sqlite", dbDSN(path))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	db.SetMaxOpenConns(1)
	if _, setupErr := db.Exec(schema); setupErr != nil {
		t.Fatal(setupErr)
	}

	attempts := 0
	err = withTx(db, func(tx *sql.Tx) error {
		attempts++
		_, insertErr := tx.Exec("INSERT INTO sessions(id) VALUES('s')")
		return insertErr
	})
	if err == nil {
		t.Fatal("withTx: want error")
	}
	if attempts != 1 {
		t.Fatalf("attempts = %d, want 1", attempts)
	}
}
