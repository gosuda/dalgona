package tui

import (
	"fmt"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func TestFollowupNoticeKeepsDockHeight(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, resultMsg{op: "new", value: &rpc.ThreadStartResponse{Thread: rpc.Thread{ID: "t1"}}})
	m.queue = []string{"a", "b"}
	before := len(m.dockLines())
	m, _ = drive(t, m, resultMsg{op: "new", value: &rpc.ThreadStartResponse{Thread: rpc.Thread{ID: "t2"}}})
	if got := len(m.dockLines()); got != before {
		t.Fatalf("dock height after the follow-up notice = %d, want %d", got, before)
	}
}

func assertShown(t *testing.T, m model, text string) {
	t.Helper()
	if strings.Contains(strings.Join(strings.Fields(stripANSI(m.View().Content)), " "), text) {
		return
	}
	for _, entry := range m.out.entries {
		if strings.Contains(strings.Join(strings.Fields(stripANSI(entry.text)), " "), text) {
			return
		}
	}
	t.Fatalf("%q is neither in the view nor in a committed row:\n%s", text, m.View().Content)
}

func liveModel(t *testing.T, now *time.Time) model {
	t.Helper()
	caller := new(fakeCaller)
	m := newTestModel(caller, Options{})
	m.conn.clock = func() time.Time { return *now }
	m, _ = drive(t, m, notificationMsg{method: "turn/started", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`)})
	return m
}

func cellCount(m model, text string) int {
	return m.screen.cells(text)
}

func TestStatusRowShowsWorkingTagAndElapsed(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := liveModel(t, &now)
	now = now.Add(12 * time.Second)
	row := stripANSI(m.liveRow())
	for _, want := range []string{"Working", "12s"} {
		if !strings.Contains(row, want) {
			t.Fatalf("live row %q lacks %q", row, want)
		}
	}
}

func TestStatusRowFieldsKeepCells(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := liveModel(t, &now)
	now = now.Add(9 * time.Second)
	before := stripANSI(m.liveRow())
	now = now.Add(time.Second)
	after := stripANSI(m.liveRow())
	if strings.Index(before, "Working") != strings.Index(after, "Working") {
		t.Fatalf("Working moved: %q then %q", before, after)
	}
	if cellCount(m, before) != cellCount(m, after) {
		t.Fatalf("live row width changed: %q then %q", before, after)
	}
}

func liveColumn(t *testing.T, m model, row, field string) int {
	t.Helper()
	index := strings.Index(row, field)
	if index < 0 {
		t.Fatalf("live row %q lacks %q", row, field)
	}
	return cellCount(m, row[:index])
}

func TestStatusRowFieldWidths(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := liveModel(t, &now)
	m.screen.width = 100
	saturateLiveRow(&m)
	m.thread.turnStartedAt = now.Add(-100 * time.Hour)
	for _, field := range []struct {
		name  string
		text  string
		cells int
		want  string
	}{
		{"calls", m.liveCalls(), liveCallsCells, "99+ run"},
		{"queued", m.liveQueued(), liveQueueCells, "32 queued"},
		{"plan", m.livePlan(), livePlanCells, "99+/99+ plan"},
		{"elapsed", m.liveElapsed(), elapsedCells, "99h 59m"},
	} {
		if got := strings.TrimSpace(stripANSI(field.text)); got != field.want {
			t.Fatalf("%s field = %q, want %q", field.name, got, field.want)
		}
		if got := cellCount(m, field.text); got != field.cells {
			t.Fatalf("%s field is %d cells, want %d", field.name, got, field.cells)
		}
	}
	requireWideLiveRowColumns(t, m)
	requireNarrowLiveRowColumns(t, m)
}

func requireWideLiveRowColumns(t *testing.T, m model) {
	t.Helper()
	row := stripANSI(m.liveRow())
	requireLiveColumn(t, m, row, "99+ run", 18)
	requireLiveColumn(t, m, row, "99+/99+ plan", 26)
	requireLiveColumn(t, m, row, "32 queued", 39)
	if got := m.liveFreeWidth(); got != 50 {
		t.Fatalf("free text holds %d cells at 100 columns, want 50 from column 50", got)
	}
}

func requireNarrowLiveRowColumns(t *testing.T, m model) {
	t.Helper()
	m.screen.width = 40
	row := stripANSI(m.liveRow())
	if strings.Contains(row, "queued") {
		t.Fatalf("queued field must drop at 40 columns: %q", row)
	}
	requireLiveColumn(t, m, row, "99+ run", 18)
	requireLiveColumn(t, m, row, "99+/99+ plan", 26)
	if got := m.liveFreeWidth(); got != 1 {
		t.Fatalf("free text holds %d cells at 40 columns, want 1 (column 39)", got)
	}
	joined := stripANSI(m.liveTag() + m.liveElapsed() + m.liveCalls() + m.livePlan())
	requireLiveColumn(t, m, joined, "99+ run", 17)
}

func requireLiveColumn(t *testing.T, m model, row, field string, want int) {
	t.Helper()
	if got := liveColumn(t, m, row, field); got != want {
		t.Fatalf("%s field starts at cell %d, want %d: %q", field, got, want, row)
	}
}

func TestStatusRowSeparatesOrdinaryFields(t *testing.T) {
	t.Parallel()
	for _, width := range []int{40, 100} {
		t.Run(fmt.Sprintf("width%d", width), func(t *testing.T) {
			t.Parallel()
			now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
			m := liveModel(t, &now)
			m.screen.width = width
			now = now.Add(time.Second)
			m.live.items = []liveItem{{ID: "c1", Type: "commandExecution", Command: "x", Status: "inProgress"}}
			row := stripANSI(m.liveRow())
			prefix, _, found := strings.Cut(row, "1s")
			if !found || cellCount(m, prefix) != 15 {
				t.Fatalf("elapsed field moved within the live row: %q", row)
			}
			if !strings.Contains(row, "1s 1 run") {
				t.Fatalf("live row lacks one space between elapsed and calls: %q", row)
			}
			if got := cellCount(m, row); got > width-1 {
				t.Fatalf("live row is %d cells at %d columns: %q", got, width, row)
			}
		})
	}
}

func TestStatusRowSaturatedFieldsSeparate(t *testing.T) {
	t.Parallel()
	for _, width := range []int{40, 100} {
		t.Run(fmt.Sprintf("width%d", width), func(t *testing.T) {
			t.Parallel()
			assertSaturatedLiveRow(t, width)
		})
	}
}

func assertSaturatedLiveRow(t *testing.T, width int) {
	t.Helper()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := liveModel(t, &now)
	m.screen.width = width
	saturateLiveRow(&m)
	m.thread.turnStartedAt = now.Add(-100 * time.Hour)
	row := stripANSI(m.liveRow())
	assertElapsedField(t, m, row)
	assertContains(t, row, "59m 99+ run", "run 99+/99+ plan")
	assertQueuedField(t, width, row)
	if got := cellCount(m, row); got > width-1 {
		t.Fatalf("live row is %d cells at %d columns: %q", got, width, row)
	}
}

func saturateLiveRow(m *model) {
	for i := range 100 {
		m.live.items = append(m.live.items, liveItem{ID: fmt.Sprintf("c%d", i), Type: "commandExecution", Command: "x", Status: "inProgress"})
	}
	m.queue = strings.Fields(strings.Repeat("next ", 32))
	for range 120 {
		m.live.plan = append(m.live.plan, rpc.TurnPlanStep{Step: "s", Status: "pending"})
	}
	for i := range 100 {
		m.live.plan[i].Status = "completed"
	}
}

func assertElapsedField(t *testing.T, m model, row string) {
	t.Helper()
	prefix, _, found := strings.Cut(row, "99h 59m")
	if !found || cellCount(m, prefix) != 10 {
		t.Fatalf("elapsed field moved within the live row: %q", row)
	}
}

func assertQueuedField(t *testing.T, width int, row string) {
	t.Helper()
	if width == 100 {
		assertContains(t, row, "plan 32 queued")
		return
	}
	if strings.Contains(row, "queued") {
		t.Fatalf("queued field must drop at %d columns: %q", width, row)
	}
}

func TestStatusRowTinyFreeTextKeepsFixedFields(t *testing.T) {
	t.Parallel()
	for _, free := range []int{0, 1, 2, 3} {
		t.Run(fmt.Sprintf("free%d", free), func(t *testing.T) {
			t.Parallel()
			now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
			m := liveModel(t, &now)
			m.screen.width = liveTagCells + elapsedCells + free + 1
			now = now.Add(time.Second)
			m.notice = "한글 ▪ \U000f0873 long notice"
			row := stripANSI(m.liveRow())
			prefix, _, found := strings.Cut(row, "1s")
			if !found || cellCount(m, prefix) != 15 {
				t.Fatalf("elapsed field moved with %d free cells: %q", free, row)
			}
			if got := cellCount(m, row); got > m.screen.width-1 {
				t.Fatalf("live row is %d cells with %d free cells: %q", got, free, row)
			}
		})
	}
}

func TestStatusRowBelowFortyColumns(t *testing.T) {
	t.Parallel()
	hints := []string{"", "Esc again to interrupt", "Ctrl+C again to exit"}
	for _, width := range []int{10, 17, 18, 30, 39} {
		for _, hint := range hints {
			t.Run(fmt.Sprintf("%dcols/%q", width, hint), func(t *testing.T) {
				t.Parallel()
				now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
				m := liveModel(t, &now)
				m.screen.width = width
				m.live.items = []liveItem{{ID: "a1", Type: "agentMessage", Text: strings.Repeat("tail ", 20)}}
				if hint != "" {
					m.setHint(hint, "interrupt")
				}
				now = now.Add(5 * time.Second)
				assertFrame(t, m.dockLines(), width, m.screen.height)
				if got := cellCount(m, m.liveRow()); got > width-1 {
					t.Fatalf("live row is %d cells at %d columns: %q", got, width, m.liveRow())
				}
			})
		}
	}
}

func TestNoticeShowsDuringTurn(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := liveModel(t, &now)
	for _, r := range "/new" {
		m, _ = drive(t, m, press(r, 0, string(r)))
	}
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if !strings.Contains(stripANSI(m.liveRow()), "/new is unavailable") {
		t.Fatalf("refusal is not in the live row: %q", m.liveRow())
	}
	m, _ = drive(t, m, press('x', 0, "x"))
	if strings.Contains(stripANSI(m.liveRow()), "/new is unavailable") {
		t.Fatalf("the next key press left the refusal in the live row: %q", m.liveRow())
	}
}

func TestNoticeRowFitsOneCellShort(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.screen.width = 40
	m.noticef("%s", strings.Repeat("n", 300))
	if got := cellCount(m, m.liveRow()); got > 39 {
		t.Fatalf("live row with a long notice is %d cells, want at most 39", got)
	}
}

func TestIdleLiveRowIsBlank(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	if got := strings.TrimSpace(stripANSI(m.liveRow())); got != "" {
		t.Fatalf("idle live row = %q, want blank", got)
	}
	m.thread.state = stateInterrupted
	if !strings.Contains(stripANSI(m.liveRow()), "Stopped") {
		t.Fatalf("interrupted live row lacks Stopped: %q", m.liveRow())
	}
}

func TestAwaitingInputShowsWaiting(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.thread.state = "awaiting input"
	if !strings.Contains(stripANSI(m.liveRow()), "Waiting") {
		t.Fatalf("awaiting input live row lacks Waiting: %q", m.liveRow())
	}
}

func TestHintExpiresWithItsWindow(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	_ = m.setHint("Esc again to interrupt", "interrupt")
	stale := m.hint.id
	_ = m.setHint("Esc again to rewind", "rewind")
	m.expireHint(hintExpiredMsg{id: stale})
	if m.hint.text != "Esc again to rewind" {
		t.Fatalf("a stale expiry cleared the hint: %q", m.hint.text)
	}
	m.expireHint(hintExpiredMsg{id: m.hint.id})
	if m.hint.text != "" || m.hint.pending != "" {
		t.Fatalf("the current expiry kept the hint: %#v", m.hint)
	}
}

func TestMultiLineNoticeCutInLiveRowIsCommitted(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.screen.width = 40
	notice := "first line of the notice\nsecond line of the notice"
	m.noticef("%s", notice)
	if cells := cellCount(m, m.liveText()); cells <= m.liveFreeWidth() {
		t.Fatalf("precondition: the live row shows the notice in %d cells, free width %d", cells, m.liveFreeWidth())
	}
	m.commitOverflowNotice()
	if len(m.out.entries) != 1 || m.out.entries[0].key != "note:0" {
		t.Fatalf("the cut notice was not committed as a row: %+v", m.out.entries)
	}
	if got := m.out.entries[0].text; got != notice {
		t.Fatalf("committed notice text = %q, want %q", got, notice)
	}
}

func TestActivityNamesCommandAndFileChange(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.live.items = []liveItem{{ID: "c1", Type: "commandExecution", Command: "go test ./...\nexit"}}
	if got := m.activity(); got != "Running go test ./..." {
		t.Fatalf("command activity = %q, want %q", got, "Running go test ./...")
	}
	m.live.items = []liveItem{{ID: "c1", Type: "commandExecution", Command: "go test ./...", output: []byte("ok\nPASS\n")}}
	if got := m.activity(); got != "Running go test ./... · PASS" {
		t.Fatalf("command activity with output = %q, want %q", got, "Running go test ./... · PASS")
	}
	m.live.items = []liveItem{{ID: "f1", Type: "fileChange", Changes: []rpc.FileChange{{Path: "notes.md"}}}}
	if got := m.activity(); got != "Editing notes.md" {
		t.Fatalf("file change activity = %q, want %q", got, "Editing notes.md")
	}
}

func TestHintUsesLiveRowAndAddsNoRow(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 40, Height: 8})
	before := len(m.dockLines())
	m.setHint("Esc again to interrupt", "interrupt")
	if got := len(m.dockLines()); got != before {
		t.Fatalf("dock height with a hint = %d, want %d", got, before)
	}
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 100, Height: 8})
	assertShown(t, m, "Esc again to interrupt")
}

func TestHintCoversCountFieldsAtForty(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := hintCoverModel(t, &now)
	now = now.Add(5 * time.Second)
	before := stripANSI(m.liveRow())
	elapsedAt := liveColumn(t, m, before, "5s")
	callsAt := liveColumn(t, m, before, "3 run")
	planAt := liveColumn(t, m, before, "2/5 plan")
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	hintRow := stripANSI(m.liveRow())
	if !strings.Contains(hintRow, "Esc again to interrupt") {
		t.Fatalf("hint row %q lacks the whole hint text", hintRow)
	}
	if got := cellCount(m, hintRow); got > 39 {
		t.Fatalf("hint row is %d cells, want at most 39: %q", got, hintRow)
	}
	if strings.Contains(hintRow, "3 run") || strings.Contains(hintRow, "2/5 plan") {
		t.Fatalf("count fields show under the hint: %q", hintRow)
	}
	requireLiveColumn(t, m, hintRow, "5s", elapsedAt)
	m, _ = drive(t, m, hintExpiredMsg{id: m.hint.id})
	after := stripANSI(m.liveRow())
	requireLiveColumn(t, m, after, "3 run", callsAt)
	requireLiveColumn(t, m, after, "2/5 plan", planAt)
	m, _ = drive(t, m, m.screen.spin.Tick())
	tickRow := stripANSI(m.liveRow())
	requireLiveColumn(t, m, tickRow, "3 run", callsAt)
	requireLiveColumn(t, m, tickRow, "2/5 plan", planAt)
}

func hintCoverModel(t *testing.T, now *time.Time) model {
	t.Helper()
	m := newTestModel(new(fakeCaller), Options{})
	m.conn.clock = func() time.Time { return *now }
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 8})
	m, _ = drive(t, m, notificationMsg{method: "turn/started", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`)})
	for i := range 3 {
		m.live.items = append(m.live.items, liveItem{ID: fmt.Sprintf("c%d", i), Type: "commandExecution", Command: "x", Status: "inProgress"})
	}
	for range 5 {
		m.live.plan = append(m.live.plan, rpc.TurnPlanStep{Step: "s", Status: "pending"})
	}
	for i := range 2 {
		m.live.plan[i].Status = "completed"
	}
	return m
}

func planStepModel(t *testing.T, width int, plan []rpc.TurnPlanStep, items []liveItem) model {
	t.Helper()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := liveModel(t, &now)
	m.screen.width = width
	m.live.plan = plan
	m.live.items = items
	return m
}

func liveFreeText(m model) string {
	row := stripANSI(m.liveRow())
	skip := fieldsWidth(m.liveFields(terminalWidth(m.screen.width)))
	cells := 0
	for index, r := range row {
		if cells >= skip {
			return strings.TrimSpace(row[index:])
		}
		cells += cellCount(m, string(r))
	}
	return ""
}

func decoderPlan() []rpc.TurnPlanStep {
	return []rpc.TurnPlanStep{
		{Step: "Inspect the decoder", Status: "completed"},
		{Step: "Fix the decoder", Status: "inProgress"},
		{Step: "Check the decoder", Status: "pending"},
	}
}

func TestStatusRowShowsCurrentPlanStep(t *testing.T) {
	t.Parallel()
	writing := liveItem{ID: "a1", Type: "agentMessage", streamTail: "streamed answer"}
	m := planStepModel(t, 100, decoderPlan(), []liveItem{writing})
	row := stripANSI(m.liveRow())
	requireLiveColumn(t, m, row, "1/3 plan", 26)
	if got := liveFreeText(m); got != "Fix the decoder" {
		t.Fatalf("free text = %q, want the in-progress plan step", got)
	}
	if strings.Contains(row, "streamed answer") {
		t.Fatalf("the streaming item hides the plan step: %q", row)
	}
}

func TestStatusRowRunningToolOutranksPlanStep(t *testing.T) {
	t.Parallel()
	writing := liveItem{ID: "a1", Type: "agentMessage", streamTail: "streamed answer"}
	running := liveItem{ID: "c1", Type: "commandExecution", Command: "go test ./...", Status: "inProgress"}
	for _, tc := range []struct {
		name  string
		items []liveItem
	}{
		{"tool then text", []liveItem{running, writing}},
		{"text then tool", []liveItem{writing, running}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := planStepModel(t, 100, decoderPlan(), tc.items)
			row := stripANSI(m.liveRow())
			requireLiveColumn(t, m, row, "1 run", 18)
			requireLiveColumn(t, m, row, "1/3 plan", 26)
			if got := liveFreeText(m); got != "Running go test ./..." {
				t.Fatalf("free text = %q, want the running tool", got)
			}
		})
	}
}

func TestStatusRowPlanStepSelection(t *testing.T) {
	t.Parallel()
	advisor := liveItem{ID: "v1", Type: "dynamicToolCall", Tool: "advisor", Status: "inProgress"}
	silentTool := liveItem{ID: "d1", Type: "dynamicToolCall", Status: "inProgress"}
	thought := liveItem{ID: "r1", Type: "reasoning", Text: "thought"}
	for _, tc := range []struct {
		name  string
		plan  []rpc.TurnPlanStep
		items []liveItem
		want  string
	}{
		{"first in progress beats an earlier pending step", []rpc.TurnPlanStep{{Step: "A", Status: "pending"}, {Step: "B", Status: "inProgress"}, {Step: "C", Status: "inProgress"}}, nil, "B"},
		{"first pending step when none is in progress", []rpc.TurnPlanStep{{Step: "X", Status: "completed"}, {Step: "Y", Status: "pending"}, {Step: "Z", Status: "pending"}}, nil, "Y"},
		{"completed plan shows the stream", []rpc.TurnPlanStep{{Step: "X", Status: "completed"}}, []liveItem{thought}, "thought"},
		{"empty plan shows the stream", nil, []liveItem{thought}, "thought"},
		{"empty plan and no item show nothing", nil, nil, ""},
		{"advisor call is no tool", []rpc.TurnPlanStep{{Step: "B", Status: "inProgress"}}, []liveItem{advisor}, "B"},
		{"tool without activity text keeps the plan step", []rpc.TurnPlanStep{{Step: "B", Status: "inProgress"}}, []liveItem{silentTool}, "B"},
		{"leading numbering stays", []rpc.TurnPlanStep{{Step: "2. Fix the decoder", Status: "inProgress"}}, nil, "2. Fix the decoder"},
		{"first line only", []rpc.TurnPlanStep{{Step: "Fix the decoder\nthen run the tests", Status: "inProgress"}}, nil, "Fix the decoder"},
		{"control text removed", []rpc.TurnPlanStep{{Step: "\x1b[31mFix\x1b[0m the \u202edecoder\x1b]0;pwn\x07", Status: "inProgress"}}, nil, "Fix the decoder"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := planStepModel(t, 100, tc.plan, tc.items)
			if got := liveFreeText(m); got != tc.want {
				t.Fatalf("free text = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestStatusRowPlanStepStaysOneRow(t *testing.T) {
	t.Parallel()
	plan := []rpc.TurnPlanStep{{Step: "Fix the decoder carefully\nthen run the tests", Status: "inProgress"}}
	m := planStepModel(t, 60, plan, nil)
	row := m.liveRow()
	if strings.Contains(row, "\n") {
		t.Fatalf("live row spans several rows: %q", row)
	}
	if got := cellCount(m, stripANSI(row)); got > 59 {
		t.Fatalf("live row is %d cells at 60 columns: %q", got, row)
	}
	if got := liveFreeText(m); got != "Fix the decoder ca..." {
		t.Fatalf("free text = %q, want the step cut to %d cells", got, m.liveFreeWidth())
	}
}

func TestStatusRowWaitsOutrankToolAndPlanStep(t *testing.T) {
	t.Parallel()
	running := liveItem{ID: "c1", Type: "commandExecution", Command: "go test ./...", Status: "inProgress"}
	plan := []rpc.TurnPlanStep{{Step: "Fix the decoder", Status: "inProgress"}}
	m := planStepModel(t, 120, plan, []liveItem{running})
	if got := liveFreeText(m); got != "Running go test ./..." {
		t.Fatalf("precondition: free text = %q, want the running tool", got)
	}
	retrying, _ := drive(t, m, notificationMsg{method: "dalgona/retry/wait", params: []byte(`{"threadId":"t1","turnId":"turn-1","remainingMs":125000}`)})
	if got := liveFreeText(retrying); got != "rate limited · retrying in 2m 05s · esc stops" {
		t.Fatalf("free text during a retry wait = %q", got)
	}
	answer := make(chan requestAnswer, 1)
	params := []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"c2","command":"echo hi","reason":"why"}`)
	waiting, _ := drive(t, m, serverRequestMsg{method: "item/commandExecution/requestApproval", params: params, answer: answer})
	if got := liveFreeText(waiting); got != "Waiting for your approval" {
		t.Fatalf("free text with an approval open = %q", got)
	}
}

func TestStatusRowPlanStepFollowsPlanNotifications(t *testing.T) {
	t.Parallel()
	m := notifyTurn(t, rowModel(t, 100), "turn/started", "inProgress")
	m = planUpdate(t, m, `[{"step":"Inspect the decoder","status":"completed"},{"step":"Fix the decoder","status":"inProgress"},{"step":"Check the decoder","status":"pending"}]`)
	if got := liveFreeText(m); got != "Fix the decoder" {
		t.Fatalf("free text after the first plan = %q, want Fix the decoder", got)
	}
	m = planUpdate(t, m, `[{"step":"Inspect the decoder","status":"completed"},{"step":"Fix the decoder","status":"completed"},{"step":"Check the decoder","status":"inProgress"}]`)
	if got := liveFreeText(m); got != "Check the decoder" {
		t.Fatalf("free text after the second plan = %q, want Check the decoder", got)
	}
	m = notifyTurn(t, m, "turn/completed", "completed")
	if got := liveFreeText(m); got != "" {
		t.Fatalf("free text after the turn ended = %q, want none", got)
	}
}
