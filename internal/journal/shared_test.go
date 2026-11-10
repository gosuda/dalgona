package journal

import (
	"bufio"
	"bytes"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"sync"
	"testing"
	"time"
)

const (
	childHomeEnv    = "JOURNAL_SHARED_CHILD_HOME"
	childSession    = "child"
	childEventCount = 60
)

func mustOpenShared(t *testing.T, dir string) *Store {
	t.Helper()
	store, err := OpenShared(dir)
	if err != nil {
		t.Fatalf("OpenShared: %v", err)
	}
	t.Cleanup(func() {
		_ = store.Close()
	})
	return store
}

func mustLockSession(t *testing.T, dir, session string) func() error {
	t.Helper()
	release, err := LockSession(dir, session)
	if err != nil {
		t.Fatalf("LockSession %s: %v", session, err)
	}
	return release
}

func mustRelease(t *testing.T, release func() error) {
	t.Helper()
	if err := release(); err != nil {
		t.Fatalf("release: %v", err)
	}
}

func appendOneByOne(store *Store, session string, first, count int) error {
	for n := first; n < first+count; n++ {
		seq, err := store.Append(context.Background(), session, event(n))
		if err != nil {
			return err
		}
		if got, want := strconv.FormatUint(seq, 10), strconv.Itoa(n+1); got != want {
			return fmt.Errorf("session %s seq = %s, want %s", session, got, want)
		}
	}
	return nil
}

func numberedEvents(count int) [][]byte {
	out := make([][]byte, count)
	for i := range out {
		out[i] = event(i)
	}
	return out
}

func requireBusySession(t *testing.T, dir, session string) {
	t.Helper()
	release, err := LockSession(dir, session)
	if release != nil {
		mustRelease(t, release)
	}
	if err == nil || err.Error() != "another dal process owns session "+session {
		t.Fatalf("second LockSession error = %v", err)
	}
}

func requireNoMirrorQuarantine(t *testing.T, dir string) {
	t.Helper()
	matches, err := filepath.Glob(filepath.Join(dir, "quarantine", "state-*.db*"))
	if err != nil {
		t.Fatal(err)
	}
	if len(matches) != 0 {
		t.Fatalf("state.db was quarantined: %v", matches)
	}
}

func TestOpenSharedTwoStores(t *testing.T) {
	dir := t.TempDir()
	first := mustOpenShared(t, dir)
	second := mustOpenShared(t, dir)
	releaseA := mustLockSession(t, dir, "a")
	releaseB := mustLockSession(t, dir, "b")
	requireBusySession(t, dir, "a")
	var wg sync.WaitGroup
	errs := make([]error, 2)
	wg.Go(func() { errs[0] = appendOneByOne(first, "a", 0, 40) })
	wg.Go(func() { errs[1] = appendOneByOne(second, "b", 0, 40) })
	wg.Wait()
	if err := errors.Join(errs...); err != nil {
		t.Fatal(err)
	}

	mustRelease(t, releaseA)
	releaseA = mustLockSession(t, dir, "a")
	if err := appendOneByOne(second, "a", 40, 5); err != nil {
		t.Fatalf("handoff to second store: %v", err)
	}
	mustRelease(t, releaseA)
	releaseA = mustLockSession(t, dir, "a")
	if err := appendOneByOne(first, "a", 45, 3); err != nil {
		t.Fatalf("handoff back to first store: %v", err)
	}
	mustRelease(t, releaseA)
	mustRelease(t, releaseB)
	closeStore(t, first)
	closeStore(t, second)

	store := mustOpen(t, dir)
	requireEvents(t, replayEvents(t, store, "a"), numberedEvents(48))
	requireEvents(t, replayEvents(t, store, "b"), numberedEvents(40))
	closeStore(t, store)
	requireNoMirrorQuarantine(t, dir)
}

func TestOpenSharedAcrossProcesses(t *testing.T) {
	if home := os.Getenv(childHomeEnv); home != "" {
		runSharedChild(t, home)
		return
	}
	dir := t.TempDir()
	store := mustOpenShared(t, dir)
	release := mustLockSession(t, dir, "parent")
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(exe, "-test.run=^TestOpenSharedAcrossProcesses$")
	cmd.Env = append(os.Environ(), childHomeEnv+"="+dir)
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	stdin, err := cmd.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err = cmd.Start(); err != nil {
		t.Fatal(err)
	}
	reader := bufio.NewReader(stdout)
	line, err := reader.ReadString('\n')
	if err != nil || line != "ready\n" {
		t.Fatalf("child handshake = %q, %v; stderr: %s", line, err, stderr.Bytes())
	}
	requireBusySession(t, dir, childSession)
	appendErr := appendOneByOne(store, "parent", 0, childEventCount)
	closeErr := stdin.Close()
	var childOut bytes.Buffer
	_, drainErr := io.Copy(&childOut, reader)
	waitErr := errors.Join(closeErr, drainErr, cmd.Wait())
	if err := errors.Join(appendErr, waitErr); err != nil {
		t.Fatalf("parent: %v; child stdout: %s; child stderr: %s", err, childOut.Bytes(), stderr.Bytes())
	}
	mustRelease(t, release)
	closeStore(t, store)

	exclusive := mustOpen(t, dir)
	requireEvents(t, replayEvents(t, exclusive, "parent"), numberedEvents(childEventCount))
	requireEvents(t, replayEvents(t, exclusive, childSession), numberedEvents(childEventCount))
	closeStore(t, exclusive)
	requireNoMirrorQuarantine(t, dir)
}

func runSharedChild(t *testing.T, dir string) {
	store := mustOpenShared(t, dir)
	release := mustLockSession(t, dir, childSession)
	if _, err := os.Stdout.WriteString("ready\n"); err != nil {
		t.Fatal(err)
	}
	if err := appendOneByOne(store, childSession, 0, childEventCount); err != nil {
		t.Fatal(err)
	}
	if _, err := io.Copy(io.Discard, os.Stdin); err != nil {
		t.Fatal(err)
	}
	mustRelease(t, release)
	closeStore(t, store)
}

func mustLockOwnedSession(t *testing.T, store *Store, session string) func() error {
	t.Helper()
	release, err := store.LockSession(session)
	if err != nil {
		t.Fatalf("LockSession %s: %v", session, err)
	}
	return release
}

func TestOpenSharedSkipsOwnedSession(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "sessions", "x", "journal.jsonl")
	owner := mustOpenShared(t, dir)
	release := mustLockOwnedSession(t, owner, "x")
	events := appendEvents(t, owner, "x", 3)
	committed := readFile(t, path)
	torn := append(bytes.Clone(committed), `{"seq":4,"prev":"`...)
	writeFile(t, path, torn)

	other := mustOpenShared(t, dir)
	if !bytes.Equal(readFile(t, path), torn) {
		t.Fatal("OpenShared repaired a session that another owner holds")
	}
	requireEvents(t, replayEvents(t, other, "x"), events)
	if !bytes.Equal(readFile(t, path), torn) {
		t.Fatal("Replay by a second Store repaired a session that the owning Store holds")
	}
	mustRelease(t, release)

	requireEvents(t, replayEvents(t, other, "x"), events)
	if !bytes.Equal(readFile(t, path), committed) {
		t.Fatal("Replay of an unowned session left the torn tail in place")
	}
	quarantined := filepath.Join(dir, "quarantine", "x-"+strconv.Itoa(len(committed))+".bin")
	if !bytes.Equal(readFile(t, quarantined), torn[len(committed):]) {
		t.Fatal("torn tail was not quarantined")
	}
	release = mustLockOwnedSession(t, other, "x")
	if seq, err := other.Append(context.Background(), "x", event(3)); err != nil || seq != 4 {
		t.Fatalf("Append after repair seq = %d, err = %v", seq, err)
	}
	mustRelease(t, release)
	closeStore(t, owner)
	closeStore(t, other)
}

func TestOpenSharedRepairsMirrorRowsPerSession(t *testing.T) {
	dir := t.TempDir()
	store := mustOpenShared(t, dir)
	appendEvents(t, store, "m", 3)
	appendEvents(t, store, "other", 2)
	closeStore(t, store)
	db, err := sql.Open("sqlite", dbDSN(filepath.Join(dir, "state.db")))
	if err != nil {
		t.Fatal(err)
	}
	var want string
	if err = db.QueryRow("SELECT hash FROM events WHERE session = 'm' AND seq = 2").Scan(&want); err != nil {
		t.Fatal(err)
	}
	if _, err = db.Exec("UPDATE events SET hash = ? WHERE session = 'm' AND seq = 2", genesisPrev); err != nil {
		t.Fatal(err)
	}
	if err = db.Close(); err != nil {
		t.Fatal(err)
	}

	store = mustOpenShared(t, dir)
	got, err := fetchRow(store.db, "m", 2)
	if err != nil || !got.ok || got.hash != want {
		t.Fatalf("mirror row = %+v, %v; want hash %s", got, err, want)
	}
	closeStore(t, store)
	requireNoMirrorQuarantine(t, dir)
}

func TestOpenSharedConcurrentBackups(t *testing.T) {
	dir := t.TempDir()
	stores := []*Store{mustOpenShared(t, dir), mustOpenShared(t, dir)}
	appendEvents(t, stores[0], "a", 4)
	appendEvents(t, stores[1], "b", 4)
	errs := make([]error, 2*len(stores))
	var wg sync.WaitGroup
	for i := range errs {
		wg.Go(func() {
			for range 10 {
				errs[i] = errors.Join(errs[i], stores[i%len(stores)].backupDB())
			}
		})
	}
	wg.Wait()
	if err := errors.Join(errs...); err != nil {
		t.Fatalf("concurrent backups: %v", err)
	}
	for _, store := range stores {
		closeStore(t, store)
	}
	db, err := openUsableDB(filepath.Join(dir, "state.db.bak"))
	if err != nil {
		t.Fatalf("backup is unusable: %v", err)
	}
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestOpenSharedLeavesMirrorWhilePeerIsOpen(t *testing.T) {
	dir := t.TempDir()
	store := mustOpenShared(t, dir)
	events := appendEvents(t, store, "s", 2)
	closeStore(t, store)
	peer, err := openLockFile(filepath.Join(dir, "users.lock"))
	if err != nil {
		t.Fatal(err)
	}
	if err = waitFileShared(peer); err != nil {
		t.Fatal(err)
	}
	dbPath := filepath.Join(dir, "state.db")
	garbage := bytes.Repeat([]byte("not a database "), 512)
	writeFile(t, dbPath, garbage)

	second, err := OpenShared(dir)
	if second != nil {
		closeStore(t, second)
	}
	if err == nil {
		t.Fatal("OpenShared accepted an unusable state.db while a peer is open")
	}
	if !bytes.Equal(readFile(t, dbPath), garbage) {
		t.Fatal("OpenShared replaced state.db while a peer is open")
	}
	requireNoMirrorQuarantine(t, dir)
	if err = releaseLock(peer); err != nil {
		t.Fatal(err)
	}

	store = mustOpenShared(t, dir)
	requireEvents(t, replayEvents(t, store, "s"), events)
	closeStore(t, store)
	matches, err := filepath.Glob(filepath.Join(dir, "quarantine", "state-*.db"))
	if err != nil || len(matches) == 0 {
		t.Fatalf("sole opener did not quarantine the unusable state.db: %v, %v", matches, err)
	}
}

func TestSessionReleaseHoldsRepairGate(t *testing.T) {
	dir := t.TempDir()
	store := mustOpenShared(t, dir)
	appendEvents(t, store, "g", 1)
	sdir, err := sessionDirFor(dir, "g")
	if err != nil {
		t.Fatalf("sessionDirFor: %v", err)
	}
	release := mustLockOwnedSession(t, store, "g")
	releaseGate, err := holdLock(filepath.Join(sdir, "repair.lock"))
	if err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() { done <- release() }()
	select {
	case err := <-done:
		t.Fatalf("release returned %v while a repair probe held the gate", err)
	case <-time.After(100 * time.Millisecond):
	}
	if !store.sessionLockedByUs(sdir) {
		_ = releaseGate()
		t.Fatal("release dropped ownership before the gate freed")
	}
	if err := releaseGate(); err != nil {
		t.Fatal(err)
	}
	if err := <-done; err != nil {
		t.Fatalf("release after the gate: %v", err)
	}
	if store.sessionLockedByUs(sdir) {
		t.Fatal("release left the session marked owned")
	}
	closeStore(t, store)
}

func blockRepairGate(t *testing.T, gatePath string) {
	t.Helper()
	if err := os.Remove(gatePath); err != nil && !os.IsNotExist(err) {
		t.Fatalf("clear repair gate: %v", err)
	}
	if err := os.Mkdir(gatePath, 0o700); err != nil {
		t.Fatalf("block repair gate: %v", err)
	}
}

func unblockRepairGate(t *testing.T, gatePath string) {
	t.Helper()
	if err := os.Remove(gatePath); err != nil {
		t.Fatalf("unblock repair gate: %v", err)
	}
}

func TestReleaseRetriesAfterGateFailure(t *testing.T) {
	dir := t.TempDir()
	store := mustOpenShared(t, dir)
	appendEvents(t, store, "g", 1)
	sdir, err := sessionDirFor(dir, "g")
	if err != nil {
		t.Fatalf("sessionDirFor: %v", err)
	}
	release := mustLockOwnedSession(t, store, "g")
	gatePath := filepath.Join(sdir, "repair.lock")
	blockRepairGate(t, gatePath)
	if err := release(); err == nil {
		t.Fatal("release succeeded while repair.lock was unopenable")
	}
	if !store.sessionLockedByUs(sdir) {
		t.Fatal("failed release dropped ownership")
	}
	unblockRepairGate(t, gatePath)
	if err := release(); err != nil {
		t.Fatalf("release retry: %v", err)
	}
	if store.sessionLockedByUs(sdir) {
		t.Fatal("release left the session marked owned")
	}
	if err := release(); err != nil {
		t.Fatalf("second release: %v", err)
	}
	closeStore(t, store)
}

func TestReplayConcurrentWithRelease(t *testing.T) {
	dir := t.TempDir()
	store := mustOpenShared(t, dir)
	release := mustLockOwnedSession(t, store, "s")
	appendEvents(t, store, "s", 3)
	var got [][]byte
	replayDone := make(chan error, 1)
	go func() {
		replayDone <- store.Replay("s", func(_ uint64, ev []byte) error {
			got = append(got, bytes.Clone(ev))
			return nil
		})
	}()
	releaseDone := make(chan error, 1)
	go func() { releaseDone <- release() }()
	if err := <-replayDone; err != nil {
		t.Fatalf("Replay: %v", err)
	}
	if err := <-releaseDone; err != nil {
		t.Fatalf("release: %v", err)
	}
	requireEvents(t, got, numberedEvents(3))
	closeStore(t, store)
}

func TestLockSessionWaitsForRepairProbe(t *testing.T) {
	dir := t.TempDir()
	sdir := filepath.Join(dir, "sessions", "g")
	if err := os.MkdirAll(sdir, 0o700); err != nil {
		t.Fatal(err)
	}
	releaseGate, err := holdLock(filepath.Join(sdir, "repair.lock"))
	if err != nil {
		t.Fatal(err)
	}
	releaseProbe, err := holdLock(filepath.Join(sdir, "lock"))
	if err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() {
		unlock, lockErr := LockSession(dir, "g")
		if lockErr == nil {
			lockErr = unlock()
		}
		done <- lockErr
	}()
	select {
	case lockErr := <-done:
		t.Fatalf("LockSession returned %v while a repair probe held the session", lockErr)
	case <-time.After(100 * time.Millisecond):
	}
	if err = errors.Join(releaseProbe(), releaseGate()); err != nil {
		t.Fatal(err)
	}
	if err = <-done; err != nil {
		t.Fatalf("LockSession after the probe: %v", err)
	}
}

func TestSessionsSharedReloadsMirrorRows(t *testing.T) {
	dir := t.TempDir()
	first := mustOpenShared(t, dir)
	release := mustLockSession(t, dir, "s")
	if _, err := first.Append(t.Context(), "s", event(0)); err != nil {
		t.Fatalf("Append: %v", err)
	}
	mustRelease(t, release)

	second := mustOpenShared(t, dir)
	release = mustLockSession(t, dir, "s")
	if err := appendOneByOne(second, "s", 1, 2); err != nil {
		t.Fatalf("peer append: %v", err)
	}
	mustRelease(t, release)
	closeStore(t, second)

	heads, err := first.Sessions()
	if err != nil {
		t.Fatalf("Sessions: %v", err)
	}
	if len(heads) != 1 || heads[0].ID != "s" || heads[0].HeadSeq != 3 || heads[0].Status != SessionOK {
		t.Fatalf("Sessions = %+v, want s at reloaded head 3 ok", heads)
	}
	closeStore(t, first)
}

func TestSessionsSharedKeepsLocalDamage(t *testing.T) {
	dir := t.TempDir()
	store := mustOpenShared(t, dir)
	release := mustLockSession(t, dir, "s")
	appendEvents(t, store, "s", 1)
	if err := store.sessions["s"].file.Close(); err != nil {
		t.Fatalf("sever handle: %v", err)
	}
	if _, err := store.Append(t.Context(), "s", event(5)); err == nil {
		t.Fatal("append on a severed handle succeeded")
	}
	store.sessions["s"].file = nil
	heads, err := store.Sessions()
	if err != nil {
		t.Fatalf("Sessions: %v", err)
	}
	want := SessionHead{ID: "s", HeadSeq: 1, Status: SessionDamaged, DamagedSeq: 2}
	if len(heads) != 1 || heads[0] != want {
		t.Fatalf("Sessions = %+v, want %+v", heads, want)
	}
	mustRelease(t, release)
	closeStore(t, store)
}

func TestOpenSharedListsPeerSessions(t *testing.T) {
	dir := t.TempDir()
	first := mustOpenShared(t, dir)
	second := mustOpenShared(t, dir)
	appendEvents(t, second, "late", 2)
	heads, err := first.Sessions()
	if err != nil {
		t.Fatal(err)
	}
	if len(heads) != 1 || heads[0].ID != "late" || heads[0].HeadSeq != 2 {
		t.Fatalf("Sessions = %+v, want late at seq 2", heads)
	}
	closeStore(t, first)
	closeStore(t, second)
}

func TestSessionDirForRetriesParentSync(t *testing.T) {
	dir := t.TempDir()
	injected := errors.New("injected dir sync failure")
	if _, err := sessionDirForSync(dir, "s", func(string) error { return injected }); err == nil {
		t.Fatal("sessionDirForSync with failing sync succeeded")
	}
	var synced []string
	sdir, err := sessionDirForSync(dir, "s", func(path string) error {
		synced = append(synced, path)
		return nil
	})
	if err != nil {
		t.Fatalf("sessionDirForSync retry: %v", err)
	}
	if want := filepath.Join(dir, "sessions", "s"); sdir != want {
		t.Fatalf("sdir = %s, want %s", sdir, want)
	}
	if len(synced) != 1 || synced[0] != filepath.Join(dir, "sessions") {
		t.Fatalf("synced = %v, want one parent sync", synced)
	}
}

func TestReplaySeesDamageUnderOwnSessionLock(t *testing.T) {
	dir := t.TempDir()
	store := mustOpenShared(t, dir)
	release := mustLockOwnedSession(t, store, "s")
	appendEvents(t, store, "s", 2)
	path := filepath.Join(dir, "sessions", "s", "journal.jsonl")
	committed := readFile(t, path)
	firstEnd := bytes.IndexByte(committed, '\n') + 1
	writeFile(t, path, committed[:firstEnd])
	writeFile(t, filepath.Join(dir, "sessions", "s", "damaged"), []byte("2\n"))
	var got [][]byte
	replayErr := store.Replay("s", func(_ uint64, ev []byte) error {
		got = append(got, bytes.Clone(ev))
		return nil
	})
	damage, ok := errors.AsType[*DamageError](replayErr)
	if !ok || damage.Seq != 2 {
		t.Fatalf("Replay error = %v, want damage at seq 2", replayErr)
	}
	requireEvents(t, got, [][]byte{event(0)})
	if _, err := store.Append(context.Background(), "s", event(9)); err == nil {
		t.Fatal("damaged session accepted an append")
	}
	mustRelease(t, release)
	closeStore(t, store)
}
