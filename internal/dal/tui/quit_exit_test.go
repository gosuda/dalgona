package tui

import (
	"testing"

	tea "charm.land/bubbletea/v2"
)

const finishedCommandItem = `{"threadId":"t1","turnId":"turn-1","completedAtMs":2,"item":{"type":"commandExecution","id":"i2","command":"ls","commandActions":[],"cwd":"/w","status":"completed","aggregatedOutput":"a"}}`

func TestQuitWaitsForExitRow(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m = rawUpdate(t, m, settledMsg{n: m.out.settleN})
	m.thread.id = "t1"
	m.thread.title = "Plan"
	m, _ = updateWithCommand(t, m, press('c', tea.ModCtrl, ""))
	m, _ = updateWithCommand(t, m, press('c', tea.ModCtrl, ""))
	if !m.quitting {
		t.Fatal("second Ctrl+C did not start the quit")
	}
	if m.out.printing {
		t.Fatal("quit printed before the frame settled")
	}
	if !m.out.settling {
		t.Fatal("quit did not wait for the frame to settle")
	}
	m, _ = updateWithCommand(t, m, settledMsg{n: m.out.settleN})
	if !m.out.printing {
		t.Fatal("exit row did not start a print after the frame settled")
	}
	m, command := updateWithCommand(t, m, printedMsg{})
	if !hasMsg[tea.QuitMsg](commandMsgs(command)) {
		t.Fatal("quit did not return tea.Quit after the exit row printed")
	}
	if len(m.out.pending) != 0 {
		t.Fatalf("pending rows after the exit print = %q, want none", m.out.pending)
	}
}

func TestSuspendWaitsForPendingRows(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m.thread.id = "t1"
	m = rawUpdate(t, m, notificationMsg{method: "item/completed", params: []byte(finishedCommandItem)})
	m, _ = updateWithCommand(t, m, press('z', tea.ModCtrl, ""))
	if !m.suspending || !m.leaving {
		t.Fatal("ctrl+z did not start suspending")
	}
	if m.out.printing {
		t.Fatal("suspend printed the waiting row before the frame settled")
	}
	m, _ = updateWithCommand(t, m, settledMsg{n: m.out.settleN})
	if !m.out.printing {
		t.Fatal("waiting row did not print after the frame settled")
	}
	m, command := updateWithCommand(t, m, printedMsg{})
	if m.suspending {
		t.Fatal("suspend still waits after the waiting row printed")
	}
	if !hasMsg[tea.SuspendMsg](commandMsgs(command)) {
		t.Fatal("suspend did not return tea.Suspend after the waiting row printed")
	}
}

func TestQuitWithPendingRowsQuitsAfterPrint(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m = rawUpdate(t, m, settledMsg{n: m.out.settleN})
	m.thread.id = "t1"
	m = rawUpdate(t, m, notificationMsg{method: "item/completed", params: []byte(finishedCommandItem)})
	if !m.out.printing {
		t.Fatal("completed item did not start a print")
	}
	m = rawUpdate(t, m, press('c', tea.ModCtrl, ""))
	m, _ = updateWithCommand(t, m, press('c', tea.ModCtrl, ""))
	if !m.quitting {
		t.Fatal("second Ctrl+C did not start the quit")
	}
	m = rawUpdate(t, m, printedMsg{})
	m = rawUpdate(t, m, settledMsg{n: m.out.settleN})
	m, command := updateWithCommand(t, m, printedMsg{})
	if !hasMsg[tea.QuitMsg](commandMsgs(command)) {
		t.Fatal("quit with pending rows did not return tea.Quit after the last print")
	}
	if len(m.out.pending) != 0 {
		t.Fatalf("pending rows after quit = %q, want none", m.out.pending)
	}
}
