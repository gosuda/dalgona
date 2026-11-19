package tui

import (
	"testing"
	"testing/synctest"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/vt"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const lateCursorReply = "\x1b[5;3R"

func queryInFlight(t *testing.T) model {
	t.Helper()
	m := anchorTestModel()
	if !requestsCursor(m.anchorDock()) {
		t.Fatal("missing cursor query")
	}
	if !m.anchorState().querying {
		t.Fatal("the cursor query is not in flight")
	}
	return m
}

func drainHint() {
	time.Sleep(hintWindow)
	synctest.Wait()
}

func quitReturned(command tea.Cmd) bool {
	return hasMsg[tea.QuitMsg](commandMsgs(command))
}

func suspendReturned(command tea.Cmd) bool {
	return hasMsg[tea.SuspendMsg](commandMsgs(command))
}

func finishRows(t *testing.T, m model, command tea.Cmd, returned func(tea.Cmd) bool) (model, bool) {
	t.Helper()
	for range 6 {
		if returned(command) {
			return m, true
		}
		switch {
		case m.out.settling:
			m, command = updateWithCommand(t, m, settledMsg{n: m.out.settleN})
		case m.out.printing:
			m, command = updateWithCommand(t, m, printedMsg{})
		default:
			return m, false
		}
	}
	return m, returned(command)
}

func quitAfterExitRow(t *testing.T, m model) (model, bool) {
	t.Helper()
	m, _ = updateWithCommand(t, m, press('c', tea.ModCtrl, ""))
	m, command := updateWithCommand(t, m, press('c', tea.ModCtrl, ""))
	if !m.quitting {
		t.Fatal("second Ctrl+C did not start the quit")
	}
	return finishRows(t, m, command, quitReturned)
}

func TestQuitHoldsWhileCursorQueryInFlight(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m, returned := quitAfterExitRow(t, queryInFlight(t))
		if returned {
			t.Fatal("quit returned tea.Quit while the cursor query was in flight")
		}
		if len(m.out.pending) != 0 || m.out.printing || m.out.settling {
			t.Fatal("quit held back with rows still waiting; the exit row did not print")
		}
		m, command := updateWithCommand(t, m, tea.CursorPositionMsg{X: 2, Y: 5})
		if !quitReturned(command) {
			t.Fatal("quit did not return tea.Quit after the cursor reply")
		}
		if m.anchorState().querying {
			t.Fatal("the cursor reply left the query in flight")
		}
	})
}

func TestQuitReturnsWhenCursorQueryExpires(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m, returned := quitAfterExitRow(t, queryInFlight(t))
		if returned {
			t.Fatal("quit returned tea.Quit while the cursor query was in flight")
		}
		_, command := updateWithCommand(t, m, dockQueryTimeoutMsg{query: m.anchorState().query})
		if !quitReturned(command) {
			t.Fatal("quit did not return tea.Quit after the cursor query expired")
		}
	})
}

func TestQuitIgnoresAStaleCursorQueryTimeout(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m, _ := quitAfterExitRow(t, queryInFlight(t))
		_, command := updateWithCommand(t, m, dockQueryTimeoutMsg{query: m.anchorState().query + 1})
		if quitReturned(command) {
			t.Fatal("the timeout of another query released the quit")
		}
	})
}

func TestSuspendHoldsWhileCursorQueryInFlight(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m, command := updateWithCommand(t, queryInFlight(t), press('z', tea.ModCtrl, ""))
		m, returned := finishRows(t, m, command, suspendReturned)
		if returned {
			t.Fatal("suspend returned tea.Suspend while the cursor query was in flight")
		}
		if !m.suspending || !m.leaving {
			t.Fatal("suspend gave up while the cursor query was in flight")
		}
		m, command = updateWithCommand(t, m, tea.CursorPositionMsg{X: 2, Y: 5})
		if !suspendReturned(command) {
			t.Fatal("suspend did not return tea.Suspend after the cursor reply")
		}
		if m.suspending {
			t.Fatal("suspend still waits after the cursor reply")
		}
	})
}

func TestSuspendReturnsWhenCursorQueryExpires(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m, command := updateWithCommand(t, queryInFlight(t), press('z', tea.ModCtrl, ""))
		m, _ = finishRows(t, m, command, suspendReturned)
		_, command = updateWithCommand(t, m, dockQueryTimeoutMsg{query: m.anchorState().query})
		if !suspendReturned(command) {
			t.Fatal("suspend did not return tea.Suspend after the cursor query expired")
		}
	})
}

func TestQuitAndSuspendIgnoreCursorQueryWithoutWindowHeight(t *testing.T) {
	t.Parallel()
	for _, leave := range []struct {
		name     string
		key      tea.KeyPressMsg
		presses  int
		returned func(tea.Cmd) bool
	}{
		{name: "quit", key: press('c', tea.ModCtrl, ""), presses: 2, returned: quitReturned},
		{name: "suspend", key: press('z', tea.ModCtrl, ""), presses: 1, returned: suspendReturned},
	} {
		t.Run(leave.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				m := queryInFlight(t)
				m.screen.height = 0
				var command tea.Cmd
				for range leave.presses {
					m, command = updateWithCommand(t, m, leave.key)
				}
				if !leave.returned(command) {
					t.Fatalf("%s waited for a cursor reply with a window height of 0", leave.name)
				}
			})
		})
	}
}

func TestQuitWaitsForCursorReply(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		em := vt.NewEmulator(100, 24)
		muteMode(em)
		run := startEmulatedProgram(em, em, nil)
		defer drainHint()
		defer run.stop()
		run.send(resultMsg{op: "initialize", value: &rpc.InitializeResult{UserAgent: "dalgona/test", PlatformFamily: "unix"}})
		run.send(commandDone("quit-proof"))
		run.waitFor(t, "cursor query in flight", func(string, string) bool {
			return run.read(func(m model) any { return m.anchorState().querying }).(bool)
		})
		replied := make(chan struct{})
		go func() {
			defer close(replied)
			time.Sleep(300 * time.Millisecond)
			_, _ = em.InputPipe().Write([]byte(lateCursorReply))
		}()
		run.send(press('c', tea.ModCtrl, ""))
		run.send(press('c', tea.ModCtrl, ""))
		time.Sleep(100 * time.Millisecond)
		synctest.Wait()
		select {
		case <-run.done:
			t.Fatal("the program returned before the terminal answered the cursor query")
		default:
		}
		time.Sleep(500 * time.Millisecond)
		synctest.Wait()
		select {
		case <-run.done:
		default:
			t.Fatal("the program still runs after the terminal answered the cursor query")
		}
		select {
		case <-replied:
		default:
			t.Fatal("the cursor reply reached the terminal input after the program returned")
		}
	})
}

func TestQuitReturnsWhenTheTerminalNeverAnswersTheCursorQuery(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		em := vt.NewEmulator(100, 24)
		muteMode(em)
		run := startEmulatedProgram(em, em, nil)
		defer drainHint()
		defer run.stop()
		run.send(resultMsg{op: "initialize", value: &rpc.InitializeResult{UserAgent: "dalgona/test", PlatformFamily: "unix"}})
		run.send(commandDone("quit-proof"))
		run.waitFor(t, "cursor query in flight", func(string, string) bool {
			return run.read(func(m model) any { return m.anchorState().querying }).(bool)
		})
		run.send(press('c', tea.ModCtrl, ""))
		run.send(press('c', tea.ModCtrl, ""))
		time.Sleep(900 * time.Millisecond)
		synctest.Wait()
		select {
		case <-run.done:
			t.Fatal("the program returned before the cursor query expired")
		default:
		}
		time.Sleep(150 * time.Millisecond)
		synctest.Wait()
		select {
		case <-run.done:
		default:
			t.Fatal("the program still runs one second after the cursor query went unanswered")
		}
	})
}
