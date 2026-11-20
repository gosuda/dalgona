package tui

import (
	"fmt"
	"io"
	"testing"
	"testing/synctest"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/charmbracelet/x/vt"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func rejectedReply(m *model) tea.CursorPositionMsg {
	return tea.CursorPositionMsg{X: m.anchorState().position.column + 7, Y: m.anchorState().position.row + 1}
}

func armedQuery(t *testing.T) model {
	t.Helper()
	m := anchorTestModel()
	if m.anchorDock() == nil || !m.anchorState().querying {
		t.Fatal("missing cursor query")
	}
	return m
}

func queriesCursor(msgs []tea.Msg) bool {
	for _, msg := range msgs {
		if raw, ok := msg.(tea.RawMsg); ok && fmt.Sprint(raw.Msg) == dockQueryMarker {
			return true
		}
	}
	return false
}

func deliverAll(t *testing.T, m model, msgs []tea.Msg) (model, bool) {
	t.Helper()
	asked := false
	for _, msg := range msgs {
		var next tea.Cmd
		m, next = updateWithCommand(t, m, msg)
		asked = asked || queriesCursor(commandMsgs(next))
	}
	return m, asked
}

func assertRejectionHoldsQuery(t *testing.T, change func(*model)) {
	t.Helper()
	m := armedQuery(t)
	change(&m)
	m, command := updateWithCommand(t, m, rejectedReply(&m))
	msgs := commandMsgs(command)
	if queriesCursor(msgs) || m.anchorState().querying {
		t.Fatal("a rejected reply started another query at once")
	}
	if !m.anchorState().needed {
		t.Fatal("a rejected reply abandoned the placement")
	}
	m, asked := deliverAll(t, m, msgs)
	if !asked || !m.anchorState().querying {
		t.Fatal("the client did not query again one second after a rejected reply")
	}
}

func TestRejectedReplyWaitsOneSecondBeforeAnotherQuery(t *testing.T) {
	t.Parallel()
	for _, rejection := range []struct {
		name   string
		change func(*model)
	}{
		{name: "cursor outside the dock", change: func(*model) {}},
		{name: "changed frame", change: func(m *model) { m.input.composer.SetValue("x") }},
	} {
		t.Run(rejection.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) { assertRejectionHoldsQuery(t, rejection.change) })
		})
	}
}

func TestRejectedReplyDoesNotHoldAfterRowOneReport(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m := anchorTestModel()
		m.anchorState().prints = 1
		m.anchorDock()
		m, _ = updateWithCommand(t, m, tea.CursorPositionMsg{X: 1, Y: 0})
		if !m.anchorState().querying {
			t.Fatal("a row-one report after a print did not request the position again")
		}
	})
}

func TestRejectedReplyDoesNotHoldWhenGeometryChanged(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m := armedQuery(t)
		m, _ = updateWithCommand(t, m, tea.WindowSizeMsg{Width: 30, Height: 8})
		m, _ = updateWithCommand(t, m, tea.CursorPositionMsg{X: 2, Y: 0})
		m, _ = updateWithCommand(t, m, settledMsg{n: m.out.settleN})
		if !m.anchorState().querying {
			t.Fatal("a geometry change did not request the position again at once")
		}
	})
}

func TestRejectedReplyDoesNotHoldWhenAPrintLanded(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m := armedQuery(t)
		m.anchorState().prints++
		m, _ = updateWithCommand(t, m, rejectedReply(&m))
		if !m.anchorState().querying {
			t.Fatal("a committed print did not request the position again at once")
		}
	})
}

func TestHoldAfterRejectedReplyEndsOnGeometryChange(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m := armedQuery(t)
		m, _ = updateWithCommand(t, m, rejectedReply(&m))
		if m.anchorState().querying {
			t.Fatal("a rejected reply started another query at once")
		}
		m, _ = updateWithCommand(t, m, tea.WindowSizeMsg{Width: 30, Height: 8})
		m, _ = updateWithCommand(t, m, settledMsg{n: m.out.settleN})
		if !m.anchorState().querying {
			t.Fatal("a geometry change did not end the hold")
		}
	})
}

func TestHoldAfterRejectedReplyEndsOnCommittedPrint(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m := armedQuery(t)
		m, _ = updateWithCommand(t, m, rejectedReply(&m))
		if m.anchorState().querying {
			t.Fatal("a rejected reply started another query at once")
		}
		m.anchorState().prints++
		m, _ = updateWithCommand(t, m, noopMsg{})
		if !m.anchorState().querying {
			t.Fatal("a committed print did not end the hold")
		}
	})
}

func TestStaleHoldTickDoesNotEndANewHold(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m := armedQuery(t)
		m, command := updateWithCommand(t, m, rejectedReply(&m))
		first := commandMsgs(command)
		for _, msg := range first {
			m, _ = updateWithCommand(t, m, msg)
		}
		if !m.anchorState().querying {
			t.Fatal("the first hold did not end after one second")
		}
		m, _ = updateWithCommand(t, m, rejectedReply(&m))
		for _, msg := range first {
			m, _ = updateWithCommand(t, m, msg)
		}
		if m.anchorState().querying {
			t.Fatal("the tick of an earlier hold ended the hold in force")
		}
	})
}

func TestHoldAfterARestartedQueryEndsOnItsOwnTick(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m := armedQuery(t)
		m, _ = updateWithCommand(t, m, rejectedReply(&m))
		m.anchorState().prints++
		m, _ = updateWithCommand(t, m, noopMsg{})
		if !m.anchorState().querying {
			t.Fatal("a committed print did not end the hold")
		}
		m, command := updateWithCommand(t, m, rejectedReply(&m))
		if m.anchorState().querying {
			t.Fatal("a second rejected reply started another query at once")
		}
		for _, msg := range commandMsgs(command) {
			m, _ = updateWithCommand(t, m, msg)
		}
		if !m.anchorState().querying {
			t.Fatal("the tick of the second hold did not end it")
		}
	})
}

type rejectingTerminal struct {
	asked  []time.Time
	answer int
}

func rejectCursorQueries(em *vt.Emulator, answer int) *rejectingTerminal {
	terminal := &rejectingTerminal{answer: answer}
	em.RegisterCsiHandler('n', func(params ansi.Params) bool {
		if n, _, ok := params.Param(0, 1); !ok || n != 6 {
			return false
		}
		terminal.asked = append(terminal.asked, time.Now())
		if len(terminal.asked) <= terminal.answer {
			_, _ = io.WriteString(em.InputPipe(), ansi.CursorPositionReport(9, 9))
		}
		return true
	})
	return terminal
}

func (terminal *rejectingTerminal) queries(run *emulatedRun) []time.Time {
	run.mu.Lock()
	defer run.mu.Unlock()
	return append([]time.Time(nil), terminal.asked...)
}

func TestFiftyRejectedRepliesQueryAtMostOncePerSecond(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		const answered = 50
		em := vt.NewEmulator(40, 12)
		tmuxMode(em)
		terminal := rejectCursorQueries(em, answered)
		run := startEmulatedProgram(em, em, nil)
		defer run.stop()
		run.send(resultMsg{op: "initialize", value: &rpc.InitializeResult{UserAgent: "dalgona/test", PlatformFamily: "unix"}})
		run.send(commandDone("reject"))
		run.waitFor(t, "row printed", func(h, s string) bool { return rowCount(h, s, "reject") == 1 })
		time.Sleep((answered + 5) * time.Second)
		synctest.Wait()
		asked := terminal.queries(run)
		if len(asked) <= answered {
			t.Fatalf("%d queries, want more than the %d answered ones", len(asked), answered)
		}
		for i := 1; i < len(asked); i++ {
			if gap := asked[i].Sub(asked[i-1]); gap < time.Second {
				t.Fatalf("query %d left %v after query %d, want at least one second", i+1, gap, i)
			}
		}
	})
}
