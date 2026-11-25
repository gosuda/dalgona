//go:build gui

package main

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type stubTerminals struct {
	closed  int
	session string
	thread  state.ThreadKey
}

func (s *stubTerminals) AttachedSession() (string, state.ThreadKey, bool) {
	if s.session == "" {
		return "", state.ThreadKey{}, false
	}
	return s.session, s.thread, true
}

func (s *stubTerminals) CloseSessions() {
	if s.session == "" {
		return
	}
	s.closed++
	s.session = ""
}

func TestCloseScopeListsAttachedTerminal(t *testing.T) {
	a := &application{open: map[state.WindowID]*window{}, owner: state.New(func() {}, t.TempDir())}
	stub := &stubTerminals{session: "t-1", thread: state.ThreadKey{Endpoint: "lab", Thread: "t-9"}}
	a.open["window-1"] = &window{id: "window-1", terminal: stub}
	a.open["window-2"] = &window{id: "window-2"}
	scope := a.closeScope("window-1")
	if len(scope.Terminals) != 1 || scope.Terminals[0] != "Terminal t-1 in t-9" {
		t.Fatalf("close scope terminals = %v, want the attached session", scope.Terminals)
	}
}

func TestCloseScopeWithoutSessionListsNothing(t *testing.T) {
	a := &application{open: map[state.WindowID]*window{}, owner: state.New(func() {}, t.TempDir())}
	a.open["window-1"] = &window{id: "window-1", terminal: &stubTerminals{}}
	a.open["window-2"] = &window{id: "window-2"}
	if scope := a.closeScope("window-1"); len(scope.Terminals) != 0 {
		t.Fatalf("close scope terminals = %v, want none", scope.Terminals)
	}
}

func TestWindowDestroyEndsTerminalSessions(t *testing.T) {
	stub := &stubTerminals{session: "t-1", thread: state.ThreadKey{Endpoint: "lab", Thread: "t-9"}}
	win := &window{id: "window-1", terminal: stub}
	win.closeSessions()
	if stub.closed != 1 {
		t.Fatalf("window destroy closed sessions %d times, want 1", stub.closed)
	}
	if _, _, ok := stub.AttachedSession(); ok {
		t.Fatal("the session survived window destroy")
	}
	win.closeSessions()
	if stub.closed != 1 {
		t.Fatalf("a second destroy closed sessions %d times, want still 1", stub.closed)
	}
}
