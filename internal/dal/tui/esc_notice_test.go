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
