package tui

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func escNoticeModel(state, draft string) (model, *fakeCaller) {
	caller := new(fakeCaller)
	m := newTestModel(caller, Options{})
	m.thread.id = "t1"
	m.thread.state = state
	m.input.composer.SetValue(draft)
	return m, caller
}

func TestEscNoticeNamesState(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name  string
		state string
		draft string
		want  string
	}{
		{name: "stopped", state: stateInterrupted, want: "stopped; nothing to interrupt"},
		{name: "stopped with a draft", state: stateInterrupted, draft: "keep me", want: "stopped; nothing to interrupt"},
		{name: "failed", state: stateFailed, want: "failed; nothing to interrupt"},
		{name: "failed with a draft", state: stateFailed, draft: "keep me", want: "failed; nothing to interrupt"},
		{name: "idle with a draft", state: stateIdle, draft: "keep me", want: "idle, draft kept; nothing to interrupt"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			m, caller := escNoticeModel(test.state, test.draft)
			m, command := drive(t, m, press(tea.KeyEscape, 0, ""))
			if command != nil {
				m, _ = drive(t, m, command())
			}
			if m.notice != test.want {
				t.Fatalf("notice = %q, want %q", m.notice, test.want)
			}
			if got := m.input.composer.Value(); got != test.draft {
				t.Fatalf("draft = %q, want %q", got, test.draft)
			}
			if len(caller.calls) != 0 {
				t.Fatalf("Esc sent %q, want no request", caller.calls[0].method)
			}
			assertShown(t, m, test.want)
		})
	}
}

func TestEscNoticeIdleEmptyDraftKeepsRewindHint(t *testing.T) {
	t.Parallel()
	m, caller := escNoticeModel(stateIdle, "")
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if strings.Contains(m.notice, "nothing to interrupt") {
		t.Fatalf("notice = %q on an empty idle prompt, want the rewind hint only", m.notice)
	}
	if m.hint.text != "Esc again to rewind" || len(caller.calls) != 0 {
		t.Fatalf("hint = %q after %d calls, want Esc again to rewind and no request", m.hint.text, len(caller.calls))
	}
}

func TestEscNoticeIdleEmptyDraftWithoutThreadIsSilent(t *testing.T) {
	t.Parallel()
	m, caller := escNoticeModel(stateIdle, "")
	m.thread.id = ""
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if m.notice != "" || m.hint.text != "" || len(caller.calls) != 0 {
		t.Fatalf("notice %q, hint %q, %d calls; want a silent Esc", m.notice, m.hint.text, len(caller.calls))
	}
}

func TestEscInterruptsEveryLiveState(t *testing.T) {
	t.Parallel()
	for _, state := range []string{stateRunning, stateAwaitingApproval, stateAwaitingInput} {
		t.Run(strings.ReplaceAll(state, " ", "-"), func(t *testing.T) {
			t.Parallel()
			m, caller := escNoticeModel(state, "keep me")
			m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
			if m.hint.text != "Esc again to interrupt" {
				t.Fatalf("first Esc hint = %q, want Esc again to interrupt", m.hint.text)
			}
			m, command := drive(t, m, press(tea.KeyEscape, 0, ""))
			m = runCommand(t, m, command, "the second Esc sent no request")
			if last := caller.calls[len(caller.calls)-1]; last.method != "turn/interrupt" {
				t.Fatalf("second Esc sent %q, want turn/interrupt", last.method)
			}
			if strings.Contains(m.notice, "nothing to interrupt") {
				t.Fatalf("a turn is live in state %q: notice = %q", state, m.notice)
			}
		})
	}
}

func TestEscNamesTheStateWithoutALiveTurn(t *testing.T) {
	t.Parallel()
	for state, want := range map[string]string{
		stateIdle:         "idle, draft kept; nothing to interrupt",
		stateInterrupted:  "stopped; nothing to interrupt",
		stateFailed:       "failed; nothing to interrupt",
		stateDisconnected: "offline; nothing to interrupt",
	} {
		t.Run(state, func(t *testing.T) {
			t.Parallel()
			m, caller := escNoticeModel(state, "keep me")
			m, command := drive(t, m, press(tea.KeyEscape, 0, ""))
			if command != nil {
				m, _ = drive(t, m, command())
			}
			if m.notice != want {
				t.Fatalf("notice = %q, want %q", m.notice, want)
			}
			if state != stateIdle && strings.Contains(m.notice, "idle") {
				t.Fatalf("notice %q names idle in state %q", m.notice, state)
			}
			if len(caller.calls) != 0 {
				t.Fatalf("Esc sent %q, want no request", caller.calls[0].method)
			}
		})
	}
}

func TestEscInIdlePromptWithDraftStaysInert(t *testing.T) {
	t.Parallel()
	m, caller := escNoticeModel(stateIdle, "keep me")
	for range 2 {
		m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
		if m.hint.text != "" || m.hint.pending != "" {
			t.Fatalf("hint %q, pending %q after Esc over a draft, want none", m.hint.text, m.hint.pending)
		}
		if m.list != nil {
			t.Fatalf("Esc over a draft opened the %q list", m.list.kind)
		}
		if m.notice != "idle, draft kept; nothing to interrupt" {
			t.Fatalf("notice = %q", m.notice)
		}
	}
	if got := m.input.composer.Value(); got != "keep me" || len(caller.calls) != 0 {
		t.Fatalf("draft %q after %d calls, want the draft kept and no request", got, len(caller.calls))
	}
}

func TestInterruptNamesEveryStateWithoutATurn(t *testing.T) {
	t.Parallel()
	for state, want := range map[string]string{
		stateIdle:         "idle; nothing to interrupt",
		stateInterrupted:  "stopped; nothing to interrupt",
		stateFailed:       "failed; nothing to interrupt",
		stateDisconnected: "offline; nothing to interrupt",
	} {
		m, caller := escNoticeModel(state, "")
		m.interrupt()
		if m.notice != want {
			t.Fatalf("state %q: notice = %q, want %q", state, m.notice, want)
		}
		if len(caller.calls) != 0 {
			t.Fatalf("state %q: interrupt sent %q, want no request", state, caller.calls[0].method)
		}
	}
}
