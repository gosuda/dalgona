package tui

import (
	"context"
	"errors"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

type treeCaller struct {
	fakeCaller
	tree rpc.ThreadTreeResponse
}

func (f *treeCaller) Call(ctx context.Context, method string, params, result any) error {
	if method != "dalgona/thread/tree" {
		return f.fakeCaller.Call(ctx, method, params, result)
	}
	f.calls = append(f.calls, callRecord{method: method, params: params})
	if out, ok := result.(*rpc.ThreadTreeResponse); ok {
		*out = f.tree
	}
	return nil
}

func rewindTree() rpc.ThreadTreeResponse {
	return rpc.ThreadTreeResponse{Turns: []rpc.TreeTurn{
		{TurnID: "turn-1", Preview: "first prompt", Active: true},
		{TurnID: "turn-2", Preview: "second prompt", Active: true},
		{TurnID: "turn-3", Preview: "branch prompt", Active: false},
	}}
}

func idleRewindModel(caller caller) model {
	m := newTestModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.thread.state = stateIdle
	return m
}

func TestEscEscRewindRestoresPrompt(t *testing.T) {
	t.Parallel()
	caller := &treeCaller{tree: rewindTree()}
	m := idleRewindModel(caller)
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	m, command := drive(t, m, press(tea.KeyEscape, 0, ""))
	m = runCommand(t, m, command, "second esc produced no command")
	assertContains(t, m.View().Content, "rewind to before a turn", "second prompt")
	if strings.Contains(m.View().Content, "branch prompt") {
		t.Fatalf("the rewind list shows an inactive turn:\n%s", m.View().Content)
	}
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	assertContains(t, m.View().Content, "Rewind to before this turn?", "[enter] rewind  [esc] keep")
	m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
	m = runCommand(t, m, command, "confirm produced no command")
	last := caller.calls[len(caller.calls)-1]
	params, ok := last.params.(rpc.ThreadRevertParams)
	if last.method != "thread/revert" || !ok || params.BeforeTurnID != "turn-2" {
		t.Fatalf("revert call = %#v", last)
	}
	if got := m.input.composer.Value(); got != "second prompt" {
		t.Fatalf("prompt after rewind = %q, want the row's label", got)
	}
}

func TestRewindHintInLiveRow(t *testing.T) {
	t.Parallel()
	m := rawUpdate(t, idleRewindModel(nil), tea.WindowSizeMsg{Width: 100, Height: 12})
	before := len(m.dockLines())
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if got := len(m.dockLines()); got != before {
		t.Fatalf("dock height after the rewind hint = %d, want %d", got, before)
	}
	assertShown(t, m, "Esc again to rewind")
}

func TestShellModeEscLeavesWithoutRewind(t *testing.T) {
	t.Parallel()
	caller := &treeCaller{tree: rewindTree()}
	m := idleRewindModel(caller)
	m, _ = drive(t, m, press('!', 0, "!"))
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if m.input.mode != inputPrompt {
		t.Fatalf("mode after the first esc = %v, want the prompt", m.input.mode)
	}
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if len(caller.calls) != 0 || m.list != nil {
		t.Fatalf("esc after leaving shell mode opened the rewind list: calls %#v", caller.calls)
	}
	if m.hint.pending != "rewind" || m.hint.text != "Esc again to rewind" {
		t.Fatalf("hint = %#v, want the rewind hint", m.hint)
	}
}

func TestRewindDraftNoticeAtEightyCharacters(t *testing.T) {
	t.Parallel()
	label := strings.Repeat("p", 80)
	caller := &treeCaller{tree: rpc.ThreadTreeResponse{Turns: []rpc.TreeTurn{{TurnID: "turn-1", Preview: label, Active: true}}}}
	m := idleRewindModel(caller)
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	m, command := drive(t, m, press(tea.KeyEscape, 0, ""))
	m = runCommand(t, m, command, "second esc produced no command")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
	m = runCommand(t, m, command, "confirm produced no command")
	assertShown(t, m, "Draft holds the first 80 characters of that prompt")
}

func TestEscAfterEndedTurnDoesNotRewind(t *testing.T) {
	t.Parallel()
	for _, state := range []string{stateFailed, stateInterrupted, stateDisconnected} {
		t.Run(state, func(t *testing.T) {
			t.Parallel()
			caller := &treeCaller{tree: rewindTree()}
			m := idleRewindModel(caller)
			m.thread.state = state
			m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
			if m.hint.pending == "rewind" {
				t.Fatalf("first esc in state %s set the rewind hint", state)
			}
			m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
			if len(caller.calls) != 0 || m.list != nil {
				t.Fatalf("esc in state %s opened the rewind list: calls %#v", state, caller.calls)
			}
		})
	}
}

func TestRewindWithoutThreadDoesNothing(t *testing.T) {
	t.Parallel()
	caller := &treeCaller{tree: rewindTree()}
	m := newTestModel(caller, Options{}, nil)
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if m.hint.pending == "rewind" {
		t.Fatalf("esc with no thread set the rewind hint %q", m.hint.text)
	}
	m, command := drive(t, m, press(tea.KeyEscape, 0, ""))
	if command != nil || len(caller.calls) != 0 {
		t.Fatalf("esc with no thread sent a request: command %v, calls %#v", command != nil, caller.calls)
	}
	if m.list != nil {
		t.Fatal("esc with no thread opened a list")
	}
}

func TestTurnListErrorNamesTheList(t *testing.T) {
	t.Parallel()
	m := rawUpdate(t, idleRewindModel(&treeCaller{}), tea.WindowSizeMsg{Width: 100, Height: 12})
	m, _ = drive(t, m, resultMsg{op: "rewindTree", err: errors.New("boom")})
	assertShown(t, m, "turn list failed: boom")
	if strings.Contains(m.View().Content, "rewindTree") {
		t.Fatalf("the error names the internal op:\n%s", m.View().Content)
	}
}
