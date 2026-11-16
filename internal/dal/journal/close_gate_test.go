package journal

import (
	"context"
	"strings"
	"testing"
	"time"
)

func waitClosed(t *testing.T, store *Store) {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for {
		store.mu.Lock()
		closed := store.closed
		store.mu.Unlock()
		if closed {
			return
		}
		if time.Now().After(deadline) {
			t.Fatal("Close did not close the gate")
		}
		time.Sleep(time.Millisecond)
	}
}

func TestCloseGateRejectsLateSession(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	appendEvents(t, store, "a", 1)
	gate := store.lockFor("a")
	gate.Lock()
	closeDone := make(chan error, 1)
	go func() { closeDone <- store.Close() }()
	waitClosed(t, store)
	if _, err := store.Append(context.Background(), "c", event(0)); err == nil ||
		!strings.Contains(err.Error(), "store is closed") {
		gate.Unlock()
		t.Fatalf("Append during Close = %v, want store is closed", err)
	}
	gate.Unlock()
	select {
	case err := <-closeDone:
		if err != nil {
			t.Fatalf("Close: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Close did not return")
	}
}

func TestSessionsListsSnapshotOnly(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	defer closeStore(t, store)
	appendEvents(t, store, "a", 1)
	appendEvents(t, store, "b", 1)
	gate := store.lockFor("b")
	gate.Lock()
	type result struct {
		heads []SessionHead
		err   error
	}
	done := make(chan result, 1)
	go func() {
		heads, err := store.Sessions()
		done <- result{heads, err}
	}()
	aLock := store.lockFor("a")
	deadline := time.Now().Add(10 * time.Second)
	for aLock.TryLock() {
		aLock.Unlock()
		if time.Now().After(deadline) {
			gate.Unlock()
			t.Fatal("Sessions did not take the session lock")
		}
		time.Sleep(time.Millisecond)
	}
	appendEvents(t, store, "c", 1)
	gate.Unlock()
	select {
	case res := <-done:
		if res.err != nil {
			t.Fatalf("Sessions: %v", res.err)
		}
		if len(res.heads) != 2 || res.heads[0].ID != "a" || res.heads[1].ID != "b" {
			t.Fatalf("Sessions = %+v, want only [a b]", res.heads)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Sessions did not return")
	}
}
