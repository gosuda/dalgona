package tui

import "testing"

func TestThreadCommandsRefusedMidTurn(t *testing.T) {
	t.Parallel()
	states := []struct {
		state string
		word  string
	}{
		{state: stateRunning, word: "working"},
		{state: stateAwaitingApproval, word: "waiting for approval"},
		{state: "awaiting input", word: "waiting for an answer"},
	}
	for _, state := range states {
		for _, name := range []string{"new", "resume", "fork", "compact", "tree", "revert"} {
			t.Run(state.state+" /"+name, func(t *testing.T) {
				t.Parallel()
				assertRefused(t, name, state.state, state.word)
			})
		}
	}
}

func assertRefused(t *testing.T, name, state, word string) {
	t.Helper()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.thread.state = state
	if command := m.executeSlash(name, ""); command != nil {
		t.Fatalf("/%s produced an RPC command while a turn is %s", name, word)
	}
	if len(caller.calls) != 0 {
		t.Fatalf("/%s made calls while a turn is %s: %#v", name, word, caller.calls)
	}
	want := "/" + name + " is unavailable while a turn is " + word + "; wait for it to finish or interrupt it first"
	if m.notice != want {
		t.Fatalf("notice = %q, want %q", m.notice, want)
	}
}

func TestThreadCommandsAllowedWhenIdle(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	if command := m.executeSlash("new", ""); command != nil || m.confirm == nil {
		t.Fatal("idle /new did not open the confirm panel")
	}
	if m.notice != "" {
		t.Fatalf("idle /new set notice %q", m.notice)
	}
}
