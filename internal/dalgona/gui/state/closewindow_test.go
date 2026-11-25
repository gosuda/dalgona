package state

import (
	"slices"
	"testing"
)

func seedWindowComposer(t *testing.T) (*machine, DraftKey, DraftKey) {
	t.Helper()
	m := newMachine()
	closed := DraftKey{Endpoint: "local", Window: "window-1", Thread: "t1"}
	kept := DraftKey{Endpoint: "local", Window: "window-2", Thread: "t1"}
	m.shells[closed] = []*shellState{{view: ShellRun{Ticket: 1, Phase: ShellRunning}, call: 7}}
	m.shells[kept] = []*shellState{{view: ShellRun{Ticket: 1, Phase: ShellExited}}}
	m.searches[closed] = &searchState{view: FileSearch{Ticket: 2, Phase: SearchSearching}, call: 8}
	m.searches[kept] = &searchState{view: FileSearch{Ticket: 2, Phase: SearchDone}}
	m.keywords[closed] = &keywordState{ticket: 9}
	m.keywords[kept] = &keywordState{}
	m.drafts[closed] = Draft{Text: "unsent"}
	m.queues[closed] = []FollowUp{{ID: 1, Text: "later"}}
	m.windows["window-1"] = []ThreadKey{{Endpoint: "local", Thread: "t1"}}
	m.started["window-1"] = ThreadKey{Endpoint: "local", Thread: "t1"}
	return m, closed, kept
}

func TestCloseWindowReapsPerWindowComposerState(t *testing.T) {
	m, closed, kept := seedWindowComposer(t)
	CloseWindow{Window: "window-1"}.apply(m)
	if _, ok := m.shells[closed]; ok {
		t.Fatalf("shell runs for %v survived CloseWindow", closed)
	}
	if _, ok := m.searches[closed]; ok {
		t.Fatalf("file search for %v survived CloseWindow", closed)
	}
	if _, ok := m.keywords[closed]; ok {
		t.Fatalf("keyword state for %v survived CloseWindow", closed)
	}
	if len(m.shells[kept]) != 1 || m.searches[kept] == nil || m.keywords[kept] == nil {
		t.Fatal("CloseWindow removed another window's composer state")
	}
	if _, ok := m.windows["window-1"]; ok {
		t.Fatal("the closed window is still listed")
	}
}

func TestCloseWindowCancelsPerWindowComposerCalls(t *testing.T) {
	m, _, _ := seedWindowComposer(t)
	CloseWindow{Window: "window-1"}.apply(m)
	var cancelled []uint64
	for _, e := range m.effects {
		if c, ok := e.(cancelEffect); ok {
			cancelled = append(cancelled, c.ticket)
		}
	}
	slices.Sort(cancelled)
	if !slices.Equal(cancelled, []uint64{7, 8, 9}) {
		t.Fatalf("cancelled calls %v, want [7 8 9]", cancelled)
	}
}
