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
