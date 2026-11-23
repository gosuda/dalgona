package tui

import (
	"testing"

	tea "charm.land/bubbletea/v2"
)

func idleFollowupModel(caller *fakeCaller, draft string) model {
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.input.composer.SetValue(draft)
	return m
}

func TestIdleAltEnterRunsSlashCommandWithoutTurnStart(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := idleFollowupModel(caller, "/compact")
	m, command := drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
	if command != nil {
		m, _ = drive(t, m, command())
	}
	if len(caller.calls) != 0 {
		t.Fatalf("idle alt+enter on /compact made calls %#v, want none", caller.calls)
	}
	if m.confirm == nil || m.confirm.kind != confirmCompact {
		t.Fatalf("idle alt+enter on /compact opened %#v, want the compact confirmation", m.confirm)
	}
	if len(m.queue) != 0 || m.input.composer.Value() != "" {
		t.Fatalf("queue = %q and composer %q, want no follow-up and an empty composer", queuedTexts(m.queue), m.input.composer.Value())
	}
}

func TestIdleAltEnterSendsDoubleSlashAsPrompt(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := idleFollowupModel(caller, "//a b")
	m, command := drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
	runCommand(t, m, command, "idle alt+enter on //a b sent nothing")
	requireLastPrompt(t, caller.calls, 1, "turn/start", "/a b")
}

func TestIdleAltEnterKeepsUnknownCommand(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := idleFollowupModel(caller, "/nosuch x")
	m, command := drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
	if command != nil {
		m, _ = drive(t, m, command())
	}
	if len(caller.calls) != 0 {
		t.Fatalf("idle alt+enter on /nosuch x made calls %#v, want none", caller.calls)
	}
	if want := "unknown command /nosuch; start with // to send it as a prompt"; m.notice != want {
		t.Fatalf("notice = %q, want %q", m.notice, want)
	}
	if got := m.input.composer.Value(); got != "/nosuch x" || len(m.queue) != 0 {
		t.Fatalf("composer = %q and queue %q, want the draft kept and no follow-up", got, queuedTexts(m.queue))
	}
}

func TestRunningAltEnterQueuesSlashDraftAsTyped(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := idleFollowupModel(caller, "/new now")
	m.thread.state = stateRunning
	m.thread.turnID = "turn-1"
	m, _ = drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
	if len(caller.calls) != 0 || len(m.queue) != 1 || m.queue[0].text != "/new now" {
		t.Fatalf("calls %#v and queue %q, want /new now queued and no call", caller.calls, queuedTexts(m.queue))
	}
}

func TestAltEnterReadsSlashRuleInEveryStateWithNoTurnRunning(t *testing.T) {
	t.Parallel()
	for _, state := range []string{stateIdle, stateInterrupted, stateFailed, stateDisconnected} {
		t.Run(state, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := idleFollowupModel(caller, "/compact")
			m.thread.state = state
			m, command := drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
			if command != nil {
				m, _ = drive(t, m, command())
			}
			if len(caller.calls) != 0 || len(m.queue) != 0 {
				t.Fatalf("%s alt+enter on /compact made calls %#v and queue %q, want neither", state, caller.calls, queuedTexts(m.queue))
			}
			if m.confirm == nil || m.confirm.kind != confirmCompact {
				t.Fatalf("%s alt+enter on /compact opened %#v, want the compact confirmation", state, m.confirm)
			}
		})
	}
}

func TestIdleAltEnterQueuesPlainDraftAsFollowup(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := idleFollowupModel(caller, "hello there")
	m, command := drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
	if want := "queued follow-up 1 of 32"; m.notice != want {
		t.Fatalf("notice = %q, want %q", m.notice, want)
	}
	runCommand(t, m, command, "idle alt+enter on plain text started nothing")
	requireLastPrompt(t, caller.calls, 1, "turn/start", "hello there")
}

func TestIdleAltEnterRunsPaletteCompletion(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := idleFollowupModel(caller, "")
	m = typeKeys(t, m, "/mo")
	if m.input.popup == nil || len(m.input.popup.items) == 0 {
		t.Fatalf("palette did not open for /mo: %+v", m.input.popup)
	}
	m, command := drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
	if command != nil {
		m, _ = drive(t, m, command())
	}
	if got := m.input.composer.Value(); got != "/model " {
		t.Fatalf("idle alt+enter on /mo gave %q, want %q", got, "/model ")
	}
	if len(caller.calls) != 0 || len(m.queue) != 0 {
		t.Fatalf("calls %#v and queue %q, want neither", caller.calls, queuedTexts(m.queue))
	}
}

func TestIdleAltEnterContinuesLineAfterBackslash(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := idleFollowupModel(caller, "//example\\")
	m, command := drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
	if command != nil {
		m, _ = drive(t, m, command())
	}
	if got, want := m.input.composer.Value(), "//example\n"; got != want {
		t.Fatalf("idle alt+enter on a trailing backslash gave %q, want %q", got, want)
	}
	if len(caller.calls) != 0 || len(m.queue) != 0 {
		t.Fatalf("calls %#v and queue %q, want neither", caller.calls, queuedTexts(m.queue))
	}
}
