package tui

import (
	"fmt"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
)

const (
	narrowCountdown = "rate limited · retrying in 2m 04s · esc stops"
	narrowNotice    = "stream reset by peer: connection closed"
	narrowHint      = "Ctrl+C again to exit"
	runningHead     = "▪ Working      8s"
	waitingHead     = "  Waiting      8s"
	failedHead      = "  Failed  "
)

type narrowRow struct {
	width int
	want  string
}

func narrowRunningModel(t *testing.T, width int) model {
	t.Helper()
	now := new(time.Time)
	*now = time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := liveModel(t, now)
	m.screen.width = width
	*now = now.Add(8 * time.Second)
	return m
}

func narrowRetryModel(t *testing.T, width int) model {
	t.Helper()
	m := narrowRunningModel(t, width)
	m, _ = drive(t, m, notificationMsg{method: "dalgona/retry/wait", params: []byte(`{"threadId":"t1","turnId":"turn-1","remainingMs":124000}`)})
	return m
}

func requireLiveRow(t *testing.T, m model, want string) {
	t.Helper()
	assertFrame(t, m.dockLines(), m.screen.width, m.screen.height)
	if got := stripANSI(m.liveRow()); got != want {
		t.Fatalf("live row at %d columns = %q, want %q", m.screen.width, got, want)
	}
}

func TestLiveRowCountdownFitsFreeText(t *testing.T) {
	t.Parallel()
	for _, row := range []narrowRow{
		{20, runningHead + ".."},
		{40, runningHead + "rate limited · retr..."},
		{60, runningHead + "rate limited · retrying in 2m 04s · esc..."},
		{100, runningHead + strings.Repeat(" ", 32) + narrowCountdown},
	} {
		t.Run(fmt.Sprintf("%dcols", row.width), func(t *testing.T) {
			t.Parallel()
			requireLiveRow(t, narrowRetryModel(t, row.width), row.want)
		})
	}
}

func TestLiveRowHintFitsFreeText(t *testing.T) {
	t.Parallel()
	for _, row := range []narrowRow{
		{20, runningHead + ".."},
		{40, runningHead + narrowHint},
		{60, runningHead + narrowHint},
		{100, runningHead + narrowHint},
	} {
		t.Run(fmt.Sprintf("%dcols", row.width), func(t *testing.T) {
			t.Parallel()
			m, _ := drive(t, narrowRunningModel(t, row.width), press('c', tea.ModCtrl, ""))
			requireLiveRow(t, m, row.want)
		})
	}
}

func TestLiveRowIdleHintFitsFreeText(t *testing.T) {
	t.Parallel()
	blank := strings.Repeat(" ", 17)
	for _, row := range []narrowRow{
		{20, "Ctrl+C again to ..."},
		{40, blank + narrowHint},
		{60, blank + narrowHint},
		{100, blank + narrowHint},
	} {
		t.Run(fmt.Sprintf("%dcols", row.width), func(t *testing.T) {
			t.Parallel()
			m := newTestModel(nil, Options{})
			m.screen.width = row.width
			m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
			requireLiveRow(t, m, row.want)
		})
	}
}

func TestLiveRowActivityFitsFreeText(t *testing.T) {
	t.Parallel()
	for _, row := range []narrowRow{
		{20, waitingHead + ".."},
		{40, waitingHead + "Waiting for your ap..."},
		{60, waitingHead + "Waiting for your approval"},
		{100, waitingHead + strings.Repeat(" ", 32) + "Waiting for your approval"},
	} {
		t.Run(fmt.Sprintf("%dcols", row.width), func(t *testing.T) {
			t.Parallel()
			m := narrowRunningModel(t, row.width)
			m.thread.state = stateAwaitingApproval
			requireLiveRow(t, m, row.want)
		})
	}
}

func TestLiveRowNoticeFitsFreeText(t *testing.T) {
	t.Parallel()
	for _, row := range []struct {
		narrowRow
		committed bool
	}{
		{narrowRow{20, runningHead + ".."}, true},
		{narrowRow{40, runningHead + "stream reset by pee..."}, true},
		{narrowRow{60, runningHead + narrowNotice}, false},
		{narrowRow{100, runningHead + strings.Repeat(" ", 32) + narrowNotice}, false},
	} {
		t.Run(fmt.Sprintf("%dcols", row.width), func(t *testing.T) {
			t.Parallel()
			m := narrowRunningModel(t, row.width)
			m.errorf("%s", narrowNotice)
			m.commitOverflowNotice()
			requireLiveRow(t, m, row.want)
			requireNoticeRows(t, m, row.committed)
		})
	}
}

func requireNoticeRows(t *testing.T, m model, committed bool) {
	t.Helper()
	want := 0
	if committed {
		want = 1
	}
	if len(m.out.entries) != want {
		t.Fatalf("committed rows at %d columns = %+v, want %d", m.screen.width, m.out.entries, want)
	}
	if committed && strings.Join(strings.Fields(m.out.entries[0].text), " ") != narrowNotice {
		t.Fatalf("committed notice = %q, want the full text %q", m.out.entries[0].text, narrowNotice)
	}
}

func TestLiveRowFailedNoticeTakesBlankElapsedCells(t *testing.T) {
	t.Parallel()
	for _, row := range []narrowRow{
		{20, failedHead + "stream..."},
		{40, failedHead + "stream reset by peer: conn..."},
		{60, failedHead + narrowNotice},
	} {
		t.Run(fmt.Sprintf("%dcols", row.width), func(t *testing.T) {
			t.Parallel()
			m := newTestModel(nil, Options{})
			m.screen.width = row.width
			m.thread.state = stateFailed
			m.errorf("%s", narrowNotice)
			requireLiveRow(t, m, row.want)
		})
	}
}

func TestLiveRowKeepsFieldsThatShowText(t *testing.T) {
	t.Parallel()
	calls := narrowRetryModel(t, 40)
	calls.live.items = []liveItem{{ID: "c1", Type: "commandExecution", Command: "x", Status: "inProgress"}}
	for i := 2; i <= 3; i++ {
		calls.live.items = append(calls.live.items, liveItem{ID: fmt.Sprintf("c%d", i), Type: "commandExecution", Command: "x", Status: "inProgress"})
	}
	requireLiveRow(t, calls, runningHead+" 3 run  rate limite...")
	plan := narrowRetryModel(t, 40)
	plan.live.plan = decoderPlan()
	requireLiveRow(t, plan, runningHead+strings.Repeat(" ", 8)+" 1/3 plan    .")
}
