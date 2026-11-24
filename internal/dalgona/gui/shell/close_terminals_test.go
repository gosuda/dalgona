//go:build gui

package shell

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func TestCloseListsAttachedTerminalSessions(t *testing.T) {
	h := newHarness(t)
	v := state.Snapshot{}.View()
	if !h.shell.Closing(v, CloseScope{Terminals: []string{"Terminal t-1 in Fix the parser"}}) {
		t.Fatal("close with an attached terminal session proceeded without asking")
	}
	h.frame(v)
	for _, want := range []string{"Close and detach the terminal sessions?", "Terminal t-1 in Fix the parser", "Stop terminal closes a process explicitly"} {
		if !h.has(want) {
			t.Fatalf("close card misses %q", want)
		}
	}
}

func TestCloseWithoutSessionsAndContentDoesNotAsk(t *testing.T) {
	h := newHarness(t)
	v := state.Snapshot{}.View()
	if h.shell.Closing(v, CloseScope{}) {
		t.Fatal("close without decisions, drafts, or sessions asked")
	}
}

func TestCloseTitlesMixedDecisionsAndTerminals(t *testing.T) {
	h := newHarness(t)
	v := pendingView()
	h.settle(v)
	if !h.shell.Closing(v, CloseScope{Terminals: []string{"Terminal t-1 in Fix the parser"}}) {
		t.Fatal("mixed close proceeded without asking")
	}
	h.frame(v)
	if !h.has("Close with pending decisions, active terminal sessions?") {
		t.Fatal("mixed close title names both categories")
	}
}
