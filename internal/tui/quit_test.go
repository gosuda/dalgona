package tui

import (
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func commandMsgs(command tea.Cmd) []tea.Msg {
	if command == nil {
		return nil
	}
	msg := command()
	batch, ok := msg.(tea.BatchMsg)
	if !ok {
		return []tea.Msg{msg}
	}
	var msgs []tea.Msg
	for _, next := range batch {
		msgs = append(msgs, commandMsgs(next)...)
	}
	return msgs
}

func hasMsg[T tea.Msg](msgs []tea.Msg) bool {
	for _, msg := range msgs {
		if _, ok := msg.(T); ok {
			return true
		}
	}
	return false
}

func runningModel(client caller) model {
	m := newTestModel(client, Options{})
	m.thread.id = "t1"
	m.thread.turnID = "turn-1"
	m.thread.state = stateRunning
	return m
}

func TestEscTwiceInterrupts(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := runningModel(caller)
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if len(caller.calls) != 0 {
		t.Fatalf("first Esc sent %q, want no request", caller.calls[0].method)
	}
	if m.hint.text != "Esc again to interrupt" {
		t.Fatalf("first Esc hint = %q", m.hint.text)
	}
	m, command := drive(t, m, press(tea.KeyEscape, 0, ""))
	runCommand(t, m, command, "second Esc did not interrupt the turn")
	if last := caller.calls[len(caller.calls)-1]; last.method != "turn/interrupt" {
		t.Fatalf("second Esc sent %q, want turn/interrupt", last.method)
	}
}

func TestEscHintExpires(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := runningModel(caller)
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	m, _ = drive(t, m, hintExpiredMsg{id: m.hint.id})
	if m.hint.text != "" {
		t.Fatalf("hint = %q after its window ended", m.hint.text)
	}
	_, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if len(caller.calls) != 0 {
		t.Fatalf("Esc after the window sent %q, want no request", caller.calls[0].method)
	}
}

func TestRateLimitWaitInterruptsOnFirstEsc(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := runningModel(caller)
	m.live.retryUntil = m.now().Add(90 * time.Second)
	m, command := drive(t, m, press(tea.KeyEscape, 0, ""))
	m = runCommand(t, m, command, "Esc during a rate-limit wait did not interrupt")
	if last := caller.calls[len(caller.calls)-1]; last.method != "turn/interrupt" {
		t.Fatalf("Esc sent %q, want turn/interrupt", last.method)
	}
}

func TestCtrlCWindow(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	if m.quitting {
		t.Fatal("first Ctrl+C quit")
	}
	if m.hint.text != "Ctrl+C again to exit" {
		t.Fatalf("first Ctrl+C hint = %q", m.hint.text)
	}
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	if !m.quitting {
		t.Fatal("second Ctrl+C inside the window did not quit")
	}
}

func TestCtrlCWindowExpires(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	m, _ = drive(t, m, hintExpiredMsg{id: m.hint.id})
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	if m.quitting {
		t.Fatal("Ctrl+C after the window quit")
	}
}

func TestQuitWithoutSizeReturnsAtOnce(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m = rawUpdate(t, m, resultMsg{op: "initialize", value: &rpc.InitializeResult{UserAgent: "dalgona/1.2.3"}})
	m.commit("probe", []string{"row waiting for a window"})
	m = rawUpdate(t, m, press('c', tea.ModCtrl, ""))
	var command tea.Cmd
	m, command = updateWithCommand(t, m, press('c', tea.ModCtrl, ""))
	if !m.quitting {
		t.Fatal("second Ctrl+C did not start the quit")
	}
	if len(m.out.pending) == 0 {
		t.Fatal("the waiting row was lost before the quit")
	}
	if !hasMsg[tea.QuitMsg](commandMsgs(command)) {
		t.Fatal("Update did not return tea.Quit with no window height")
	}
}

func updateWithCommand(t *testing.T, m model, msg tea.Msg) (model, tea.Cmd) {
	t.Helper()
	updated, command := m.Update(msg)
	next, ok := updated.(model)
	if !ok {
		t.Fatalf("Update returned %T, want model", updated)
	}
	return next, command
}

func TestQuitCommandWaitsForPrintedRows(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m.commit("probe", []string{"row waiting for a settled frame"})
	m, _ = updateWithCommand(t, m, press('c', tea.ModCtrl, ""))
	m, command := updateWithCommand(t, m, press('c', tea.ModCtrl, ""))
	if !m.quitting {
		t.Fatal("second Ctrl+C did not start the quit")
	}
	for range 6 {
		if hasMsg[tea.QuitMsg](commandMsgs(command)) {
			if len(m.out.pending) != 0 {
				t.Fatal("quit returned before the waiting row printed")
			}
			return
		}
		switch {
		case m.out.settling:
			m, command = updateWithCommand(t, m, settledMsg{n: m.out.settleN})
		case m.out.printing:
			m, command = updateWithCommand(t, m, printedMsg{})
		default:
			t.Fatalf("quit held back with nothing to wait for; pending %d", len(m.out.pending))
		}
	}
	t.Fatal("quit never returned tea.Quit after the rows printed")
}

func TestLeavingViewIsOneBlankRow(t *testing.T) {
	t.Parallel()
	quitting := newTestModel(nil, Options{})
	quitting = rawUpdate(t, quitting, press('c', tea.ModCtrl, ""))
	quitting = rawUpdate(t, quitting, press('c', tea.ModCtrl, ""))
	suspending := rawUpdate(t, newTestModel(nil, Options{}), press('z', tea.ModCtrl, ""))
	for name, m := range map[string]model{"quit": quitting, "suspend": suspending} {
		view := m.View()
		if view.Content != " " {
			t.Errorf("%s leaving view = %q, want one space", name, view.Content)
		}
		if view.ProgramStatus == nil || view.ProgramStatus.State != tea.ProgramStateIdle {
			t.Errorf("%s leaving view program status = %+v, want idle", name, view.ProgramStatus)
		}
	}
}

func TestSuspendWithoutSizeReturnsAtOnce(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.commit("probe", []string{"row waiting for a window"})
	updated, command := m.Update(press('z', tea.ModCtrl, ""))
	next := updated.(model)
	if !next.leaving {
		t.Fatal("ctrl+z did not start leaving")
	}
	if !hasMsg[tea.SuspendMsg](commandMsgs(command)) {
		t.Fatal("ctrl+z with no window height did not suspend at once")
	}
	if next.finishSuspend() != nil {
		t.Fatal("a second suspend started after the first returned")
	}
}

func TestResumeClearsLeaving(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.suspend()
	m = rawUpdate(t, m, tea.ResumeMsg{})
	if m.leaving {
		t.Fatal("ResumeMsg left the client leaving")
	}
}
