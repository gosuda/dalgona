package tui

import (
	"fmt"
	"slices"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/dal/rpc"
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
		{40, runningHead + " rate limited · ret..."},
		{60, runningHead + " rate limited · retrying in 2m 04s · es..."},
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
		{20, "Ctrl+C again to ..."},
		{40, runningHead + " " + narrowHint},
		{60, runningHead + " " + narrowHint},
		{100, runningHead + " " + narrowHint},
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
		{40, waitingHead + " Waiting for your a..."},
		{60, waitingHead + " Waiting for your approval"},
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
		{narrowRow{40, runningHead + " stream reset by pe..."}, true},
		{narrowRow{60, runningHead + " " + narrowNotice}, false},
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

func TestLiveRowKeepsFieldsThatShowTextBesideActivity(t *testing.T) {
	t.Parallel()
	calls := narrowRunningModel(t, 40)
	calls.live.items = []liveItem{{ID: "c1", Type: "commandExecution", Command: "x", Status: "inProgress"}}
	for i := 2; i <= 3; i++ {
		calls.live.items = append(calls.live.items, liveItem{ID: fmt.Sprintf("c%d", i), Type: "commandExecution", Command: "x", Status: "inProgress"})
	}
	requireLiveRow(t, calls, runningHead+" 3 run  Running x")
	plan := narrowRunningModel(t, 40)
	plan.live.plan = []rpc.TurnPlanStep{{Step: "Fix the decoder", Status: "inProgress"}}
	requireLiveRow(t, plan, runningHead+strings.Repeat(" ", 8)+" 0/1 plan    .")
}

func TestLiveRowDropsQueuedThenPlanThenCallsInANarrowWindow(t *testing.T) {
	t.Parallel()
	for _, row := range []struct {
		width int
		shown []string
	}{
		{100, []string{"3 run", "1/3 plan", "1 queued"}},
		{60, []string{"3 run", "1/3 plan", "1 queued"}},
		{40, []string{"3 run", "1/3 plan"}},
		{30, []string{"3 run"}},
		{24, nil},
	} {
		t.Run(fmt.Sprintf("%dcols", row.width), func(t *testing.T) {
			t.Parallel()
			m := narrowRunningModel(t, row.width)
			for i := 1; i <= 3; i++ {
				m.live.items = append(m.live.items, liveItem{ID: fmt.Sprintf("c%d", i), Type: "commandExecution", Command: "x", Status: "inProgress"})
			}
			m.live.plan = decoderPlan()
			m.queue = []string{"next"}
			got := stripANSI(m.liveRow())
			assertFrame(t, m.dockLines(), row.width, m.screen.height)
			for _, field := range []string{"3 run", "1/3 plan", "1 queued"} {
				if shown := slices.Contains(row.shown, field); strings.Contains(got, field) != shown {
					t.Fatalf("live row at %d columns = %q, want %q shown = %v", row.width, got, field, shown)
				}
			}
		})
	}
}

func TestLiveRowMessageCoversCountFieldsToKeepItsCells(t *testing.T) {
	t.Parallel()
	for _, row := range []narrowRow{
		{40, runningHead + " rate limited · ret..."},
		{60, runningHead + " 1 run  rate limited · retrying in 2m 0..."},
	} {
		t.Run(fmt.Sprintf("%dcols", row.width), func(t *testing.T) {
			t.Parallel()
			m := narrowRetryModel(t, row.width)
			m.live.items = []liveItem{{ID: "c1", Type: "commandExecution", Command: "x", Status: "inProgress"}}
			m.live.plan = decoderPlan()
			m.queue = []string{"next"}
			requireLiveRow(t, m, row.want)
		})
	}
}

func TestLiveRowEveryMessageCoversThePlanAtForty(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		set  func(m *model)
		want string
	}{
		{"countdown", func(m *model) {
			m.live.retryUntil = m.now().Add(124 * time.Second)
		}, runningHead + " rate limited · ret..."},
		{"notice", func(m *model) { m.noticef("%s", narrowNotice) }, runningHead + " stream reset by pe..."},
		{"approval wait", func(m *model) {
			m.thread.state = stateAwaitingApproval
		}, waitingHead + " Waiting for your a..."},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := narrowRunningModel(t, 40)
			m.live.plan = decoderPlan()
			tc.set(&m)
			requireLiveRow(t, m, tc.want)
			if got := m.liveFreeWidth(); got != 21 {
				t.Fatalf("free text holds %d cells under the %s, want 21", got, tc.name)
			}
		})
	}
}

func TestLiveRowSettingsErrorCoversThePlanAtForty(t *testing.T) {
	t.Parallel()
	m := openSettingsPanel(t, newTestModel(new(failingSaveCaller), Options{}), "ask")
	m.screen.width = 40
	m.live.plan = decoderPlan()
	m.settings.saveErr = narrowNotice
	row := stripANSI(m.liveRow())
	if row != narrowNotice {
		t.Fatalf("live row with a settings error and a plan = %q, want %q", row, narrowNotice)
	}
}

func TestLiveRowQuestionWaitCoversThePlanAtForty(t *testing.T) {
	t.Parallel()
	m, _ := openQuestion(t, questionParamsJSON)
	m.screen.width = 40
	m.live.plan = decoderPlan()
	row := stripANSI(m.liveRow())
	if want := "  Waiting Waiting for your answer"; row != want {
		t.Fatalf("live row while a question waits with a plan = %q, want %q", row, want)
	}
}

func TestLiveRowKeyHintCoversTheHeadBelowFortyColumns(t *testing.T) {
	t.Parallel()
	const hint = "Esc again to interrupt"
	for _, row := range []narrowRow{
		{20, "Esc again to int..."},
		{39, hint},
		{40, runningHead + hint},
		{41, runningHead + " " + hint},
	} {
		t.Run(fmt.Sprintf("%dcols", row.width), func(t *testing.T) {
			t.Parallel()
			m := narrowRunningModel(t, row.width)
			m.setHint(hint, "interrupt")
			requireLiveRow(t, m, row.want)
		})
	}
}

func TestNoticeCommitFollowsTheLiveRowLayout(t *testing.T) {
	t.Parallel()
	for _, row := range []struct {
		name      string
		notice    string
		committed bool
	}{
		{"fits the free cells", strings.Repeat("n", 21), false},
		{"one cell over", strings.Repeat("n", 22), true},
	} {
		t.Run(row.name, func(t *testing.T) {
			t.Parallel()
			m := narrowRunningModel(t, 40)
			m.live.plan = decoderPlan()
			m.errorf("%s", row.notice)
			m.commitOverflowNotice()
			requireNoticeRowsFor(t, m, row.committed)
		})
	}
}

func requireNoticeRowsFor(t *testing.T, m model, committed bool) {
	t.Helper()
	if got := len(m.out.entries) == 1; got != committed {
		t.Fatalf("committed rows = %+v, want committed = %v", m.out.entries, committed)
	}
}

func TestCutTextBelowThreeCellsShowsOneDotPerCell(t *testing.T) {
	t.Parallel()
	screen := screenState{method: ansi.GraphemeWidth}
	for _, tc := range []struct {
		text string
		n    int
		want string
	}{
		{"ab", 2, "ab"},
		{"a", 1, "a"},
		{"한", 2, "한"},
		{"abc", 2, ".."},
		{"ab", 1, "."},
		{"한글", 2, ".."},
		{"abc", 0, ""},
		{"", 2, ""},
		{"abcdef", 5, "ab..."},
		{"abc", 3, "abc"},
	} {
		if got := screen.cutText(tc.text, tc.n); got != tc.want {
			t.Errorf("cutText(%q, %d) = %q, want %q", tc.text, tc.n, got, tc.want)
		}
	}
}
