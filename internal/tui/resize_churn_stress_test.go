package tui

import (
	"fmt"
	"slices"
	"strings"
	"testing"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

var stormSizes = []tea.WindowSizeMsg{
	{Width: 0, Height: 0},
	{Width: 1, Height: 1},
	{Width: 2, Height: 2},
	{Width: 40, Height: 8},
	{Width: 100, Height: 24},
	{Width: 200, Height: 50},
	{Width: 10000, Height: 5000},
	{Width: 80, Height: 3},
	{Width: 30, Height: 2},
	{Width: 120, Height: 1},
}

func stormResize(t *testing.T, m model, rounds int) model {
	t.Helper()
	for i := 0; i < rounds; i++ {
		size := stormSizes[i%len(stormSizes)]
		m, _ = drive(t, m, size)
		assertStormFrame(t, m, size.Width, size.Height)
	}
	return m
}

func assertStormFrame(t *testing.T, m model, width, height int) {
	t.Helper()
	content := m.View().Content
	if height >= 2 && lineCount(content) > height {
		t.Fatalf("height %d: view has %d lines", height, lineCount(content))
	}
	if width <= 0 {
		return
	}
	limit := width
	if width >= 2 {
		limit = width - 1
	}
	screen := screenState{method: m.screen.method}
	for _, line := range strings.Split(content, "\n") {
		if cells := screen.cells(line); cells > limit {
			t.Fatalf("width %d height %d screen %dx%d printing=%v frozen=%d: line has %d cells: %.120q", width, height, m.screen.width, m.screen.height, m.out.printing, len(m.out.frozen.Content), cells, line)
		}
	}
}

func stormSeed(t *testing.T) model {
	t.Helper()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 24})
	m, _ = drive(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)})
	m, _ = drive(t, m, notificationMsg{method: "turn/started", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`)})
	m = typeText(t, m, strings.Repeat("draft ", 30))
	return m
}

func TestResizeStormKeepsFrameBounded(t *testing.T) {
	t.Parallel()
	m := stormSeed(t)
	m = stormResize(t, m, 2000)
	if m.thread.state != stateRunning {
		t.Fatalf("state after storm = %q, want running", m.thread.state)
	}
	if got := m.input.composer.Value(); !strings.HasPrefix(got, "draft") {
		t.Fatalf("composer lost draft: %q", got)
	}
}

func TestResizeStormDuringTurnAndPanels(t *testing.T) {
	t.Parallel()
	for _, open := range []string{"list", "approval", "confirm"} {
		t.Run(open, func(t *testing.T) {
			t.Parallel()
			m := stormSeed(t)
			switch open {
			case "list":
				m.openList(&listPanel{kind: "resume", title: "resume", items: []listItem{{id: "a", label: "alpha"}, {id: "b", label: "beta"}}})
			case "approval":
				m.approvals = []approval{{threadID: "t1", itemID: "c1", command: "rm -rf /", cwd: "/w", reason: "test"}}
			case "confirm":
				m.openConfirm(confirmation{kind: confirmNew, title: "Start a new thread?", action: "[enter] go"})
			}
			m = stormResize(t, m, 400)
			m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 24})
			assertStormFrame(t, m, 100, 24)
			if len(m.dockLines()) == 0 {
				t.Fatal("dock vanished during panel storm")
			}
		})
	}
}

func TestResizeStormFinalFrameMatchesDirect(t *testing.T) {
	t.Parallel()
	build := func(t *testing.T) model {
		m := newTestModel(nil, Options{})
		m, _ = drive(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"idle","activeFlags":[]},"turns":[]}}`)})
		return typeText(t, m, "hello world draft")
	}
	stormed := build(t)
	stormed, _ = drive(t, stormed, tea.WindowSizeMsg{Width: 100, Height: 24})
	last := tea.WindowSizeMsg{Width: 100, Height: 24}
	stormed, _ = drive(t, stormed, last)

	direct := build(t)
	direct, _ = drive(t, direct, last)

	if got, want := strings.Join(stormed.dockLines(), "\n"), strings.Join(direct.dockLines(), "\n"); got != want {
		t.Fatalf("dock after storm differs from direct resize to %dx%d:\nstormed:\n%s\ndirect:\n%s", last.Width, last.Height, got, want)
	}
}

func churnNotify(method, params string) tea.Msg {
	return notificationMsg{method: method, params: []byte(params)}
}

func churnTurns(t *testing.T, m model, turns, itemsPerTurn int) model {
	t.Helper()
	for turn := 0; turn < turns; turn++ {
		turnID := fmt.Sprintf("turn-%d", turn)
		m, _ = drive(t, m, churnNotify("turn/started", fmt.Sprintf(`{"threadId":"t1","turn":{"id":%q,"items":[],"status":"inProgress"}}`, turnID)))
		for item := 0; item < itemsPerTurn; item++ {
			itemID := fmt.Sprintf("c%d", item)
			m, _ = drive(t, m, churnNotify("item/started", fmt.Sprintf(`{"threadId":"t1","turnId":%q,"startedAtMs":1,"item":{"type":"commandExecution","id":%q,"command":"cmd-%02d-%02d","commandActions":[],"cwd":"/w","status":"inProgress","aggregatedOutput":null}}`, turnID, itemID, turn, item)))
			m, _ = drive(t, m, churnNotify("item/commandExecution/outputDelta", fmt.Sprintf(`{"threadId":"t1","turnId":%q,"itemId":%q,"delta":"line one\\nline two\\n"}`, turnID, itemID)))
			m, _ = drive(t, m, churnNotify("item/completed", fmt.Sprintf(`{"threadId":"t1","turnId":%q,"completedAtMs":2,"item":{"type":"commandExecution","id":%q,"command":"cmd-%02d-%02d","commandActions":[],"cwd":"/w","status":"completed","aggregatedOutput":"done","exitCode":0}}`, turnID, itemID, turn, item)))
		}
		m, _ = drive(t, m, churnNotify("turn/completed", fmt.Sprintf(`{"threadId":"t1","turn":{"id":%q,"items":[],"status":"completed"}}`, turnID)))
	}
	return m
}

func churnStatus(t *testing.T, m model, n int) model {
	t.Helper()
	for i := 0; i < n; i++ {
		m, _ = drive(t, m, churnNotify("thread/status/changed", fmt.Sprintf(`{"threadId":"t1","seq":%d,"status":{"type":"idle","activeFlags":[]}}`, i+1)))
		m, _ = drive(t, m, churnNotify("thread/tokenUsage/updated", fmt.Sprintf(`{"threadId":"t1","tokenUsage":{"last":{"inputTokens":%d}}}`, i)))
	}
	return m
}

func committedOrder(entries []entry, turns, itemsPerTurn int) []int {
	var order []int
	for _, e := range entries {
		for turn := 0; turn < turns; turn++ {
			for item := 0; item < itemsPerTurn; item++ {
				if strings.Contains(e.title, fmt.Sprintf("cmd-%02d-%02d", turn, item)) {
					order = append(order, turn*itemsPerTurn+item)
				}
			}
		}
	}
	return order
}

func TestStateChurnKeepsBuffersBounded(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 24})
	m, _ = drive(t, m, churnNotify("thread/started", `{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"idle","activeFlags":[]},"turns":[]}}`))

	m = churnTurns(t, m, 48, 25)
	m = churnStatus(t, m, 8000)

	if len(m.live.items) != 0 {
		t.Fatalf("live items leaked: %d", len(m.live.items))
	}
	if len(m.out.entries) > maxReviewEntries {
		t.Fatalf("review entries %d exceed cap %d", len(m.out.entries), maxReviewEntries)
	}
	if len(m.out.pending) != 0 {
		t.Fatalf("print queue stuck at %d rows", len(m.out.pending))
	}
	m = settleFrames(m)
	assertStormFrame(t, m, 100, 24)

	order := committedOrder(m.out.entries, 48, 25)
	if len(order) == 0 {
		t.Fatal("no command rows committed")
	}
	for i := 1; i < len(order); i++ {
		if order[i] < order[i-1] {
			t.Fatalf("transcript order violated at %d: %v", i, order[max(0, i-3):i+1])
		}
	}
}

func TestDeltaChurnStaysLinear(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 24})
	m, _ = drive(t, m, churnNotify("thread/started", `{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"idle","activeFlags":[]},"turns":[]}}`))
	m, _ = drive(t, m, churnNotify("turn/started", `{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`))
	const deltas = 8000
	chunk := strings.Repeat("x", 64)
	params := fmt.Sprintf(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":%q}`, chunk)
	for i := 0; i < deltas; i++ {
		m, _ = drive(t, m, churnNotify("item/agentMessage/delta", params))
	}
	if len(m.live.items) != 1 {
		t.Fatalf("live items = %d, want 1", len(m.live.items))
	}
	if got := len(m.live.items[0].text); got != deltas*64 {
		t.Fatalf("streamed text = %d bytes, want %d", got, deltas*64)
	}
	assertStormFrame(t, m, 100, 24)
	content := m.View().Content
	if len(content) > 100*24*4 {
		t.Fatalf("view content %d bytes unbounded for 100x24", len(content))
	}
}

func TestOutOfOrderAndDuplicateNotifications(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 24})
	m, _ = drive(t, m, churnNotify("thread/started", `{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"idle","activeFlags":[]},"turns":[]}}`))
	m, _ = drive(t, m, churnNotify("turn/started", `{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`))

	m = staleTurnChecks(t, m)

	m = dupCompletedCheck(t, m)

	m, _ = drive(t, m, churnNotify("thread/status/changed", `{"threadId":"t1","seq":1,"status":{"type":"active","activeFlags":[]}}`))
	m, _ = drive(t, m, churnNotify("thread/status/changed", `{"threadId":"t1","seq":0,"status":{"type":"idle","activeFlags":[]}}`))
	if m.thread.state != stateRunning {
		t.Fatalf("state regressed on stale seq: %q", m.thread.state)
	}

	m = foreignThreadChecks(t, m)

	m = malformedDeltaCheck(t, m)

	m, _ = drive(t, m, churnNotify("turn/completed", `{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"completed"}}`))
	if m.thread.state != stateIdle {
		t.Fatalf("final turn/completed left state %q", m.thread.state)
	}
}

func TestResizeStormWithQueuedFollowups(t *testing.T) {
	t.Parallel()
	m := stormSeed(t)
	for i := 0; i < 33; i++ {
		m.input.composer.SetValue(fmt.Sprintf("follow %d", i))
		m, _ = drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
	}
	if len(m.queue) != maxFollowups {
		t.Fatalf("queue = %d, want %d", len(m.queue), maxFollowups)
	}
	m = stormResize(t, m, 300)
	if len(m.queue) != maxFollowups {
		t.Fatalf("queue lost follow-ups in storm: %d", len(m.queue))
	}
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 24})
	m = settleFrames(m)
	assertStormFrame(t, m, 100, 24)
}

func staleTurnChecks(t *testing.T, m model) model {
	t.Helper()
	m, _ = drive(t, m, churnNotify("turn/completed", `{"threadId":"t1","turn":{"id":"turn-OLD","items":[],"status":"completed"}}`))
	if m.thread.state != stateRunning || m.thread.turnID != "turn-1" {
		t.Fatalf("stale turn/completed changed state=%q turn=%q", m.thread.state, m.thread.turnID)
	}
	m, _ = drive(t, m, churnNotify("turn/plan/updated", `{"threadId":"t1","turnId":"turn-OLD","plan":[{"step":"old","status":"pending"}]}`))
	m, _ = drive(t, m, churnNotify("dalgona/retry/wait", `{"threadId":"t1","turnId":"turn-OLD","remainingMs":60000}`))
	if !m.live.retryUntil.IsZero() {
		t.Fatal("stale retry wait opened a countdown")
	}
	m, _ = drive(t, m, churnNotify("item/agentMessage/delta", `{"threadId":"t1","turnId":"turn-OLD","itemId":"a1","delta":"stale"}`))
	if len(m.live.items) != 0 {
		t.Fatalf("stale delta created live item: %+v", m.live.items)
	}
	m, _ = drive(t, m, churnNotify("turn/plan/updated", `{"threadId":"t1","turnId":"turn-1","plan":[{"step":"current","status":"inProgress"}]}`))
	if len(m.live.plan) != 1 {
		t.Fatalf("plan for active turn not stored: %v", m.live.plan)
	}
	return m
}

func dupCompletedCheck(t *testing.T, m model) model {
	t.Helper()
	m, _ = drive(t, m, churnNotify("item/started", `{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"commandExecution","id":"c1","command":"ls","commandActions":[],"cwd":"/w","status":"inProgress","aggregatedOutput":null}}`))
	done := `{"threadId":"t1","turnId":"turn-1","completedAtMs":2,"item":{"type":"commandExecution","id":"c1","command":"ls","commandActions":[],"cwd":"/w","status":"completed","aggregatedOutput":"out","exitCode":0}}`
	before := len(m.out.entries)
	m, _ = drive(t, m, churnNotify("item/completed", done))
	m, _ = drive(t, m, churnNotify("item/completed", done))
	m = settleFrames(m)
	count := 0
	for _, entry := range m.out.entries[before:] {
		if strings.Contains(entry.title, "ls") {
			count++
		}
	}
	if count != 1 {
		t.Fatalf("duplicate item/completed printed %d rows, want 1", count)
	}
	return m
}

func foreignThreadChecks(t *testing.T, m model) model {
	t.Helper()
	entries := len(m.out.entries)
	m, _ = drive(t, m, churnNotify("thread/started", `{"thread":{"id":"other","name":"x","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`))
	m, _ = drive(t, m, churnNotify("item/completed", `{"threadId":"other","turnId":"t9","completedAtMs":2,"item":{"type":"commandExecution","id":"z","command":"zzz","commandActions":[],"cwd":"/w","status":"completed","aggregatedOutput":"x","exitCode":0}}`))
	if len(m.out.entries) != entries {
		t.Fatal("foreign thread notification printed a row")
	}
	if m.thread.id != "t1" {
		t.Fatalf("thread switched to %q", m.thread.id)
	}
	return m
}

func malformedDeltaCheck(t *testing.T, m model) model {
	t.Helper()
	for _, bad := range []string{`{`, `{"threadId":`, `{"threadId":123}`} {
		m, _ = drive(t, m, churnNotify("item/agentMessage/delta", bad))
	}
	if !strings.Contains(m.notice, "protocol error") {
		t.Fatalf("malformed delta gave no protocol error: %q", m.notice)
	}
	return m
}

func TestInterleavedResizeAndChurn(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 24})
	m, _ = drive(t, m, churnNotify("thread/started", `{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"idle","activeFlags":[]},"turns":[]}}`))
	m, _ = drive(t, m, churnNotify("turn/started", `{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`))
	d := strings.Repeat("z", 100)
	for i := 0; i < 1500; i++ {
		switch i % 4 {
		case 0:
			m, _ = drive(t, m, stormSizes[i%len(stormSizes)])
		case 1:
			m, _ = drive(t, m, churnNotify("item/agentMessage/delta", fmt.Sprintf(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":%q}`, d)))
		case 2:
			m, _ = drive(t, m, churnNotify("item/commandExecution/outputDelta", fmt.Sprintf(`{"threadId":"t1","turnId":"turn-1","itemId":"c1","delta":%q}`, d)))
		default:
			m, _ = drive(t, m, churnNotify("thread/tokenUsage/updated", fmt.Sprintf(`{"threadId":"t1","tokenUsage":{"last":{"inputTokens":%d,"outputTokens":%d}}}`, i, i*2)))
		}
		if i%200 == 0 {
			assertStormFrame(t, m, m.screen.width, m.screen.height)
		}
	}
	m = settleFrames(m)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 24})
	assertStormFrame(t, m, 100, 24)
	if !slices.Contains([]string{stateRunning, stateIdle}, m.thread.state) {
		t.Fatalf("bad state %q", m.thread.state)
	}
}

func TestResizeDuringPrintRebuildsFrozenFrame(t *testing.T) {
	t.Parallel()
	m := stormSeed(t)
	m, _ = drive(t, m, churnNotify("item/started", `{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"commandExecution","id":"c9","command":"ls","commandActions":[],"cwd":"/w","status":"inProgress","aggregatedOutput":null}}`))
	m = rawUpdate(t, m, churnNotify("item/completed", `{"threadId":"t1","turnId":"turn-1","completedAtMs":2,"item":{"type":"commandExecution","id":"c9","command":"ls","commandActions":[],"cwd":"/w","status":"completed","aggregatedOutput":"out","exitCode":0}}`))
	if !m.out.printing {
		t.Fatal("precondition: completed item did not start a print")
	}
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 40, Height: 10})
	assertStormFrame(t, m, 40, 10)
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 120, Height: 30})
	assertStormFrame(t, m, 120, 30)
}

func TestFitLongTextKeepsRunesAndEscapes(t *testing.T) {
	t.Parallel()
	screen := screenState{method: ansi.GraphemeWidth}
	plain := strings.Repeat("中文한글😀", 40000)
	styled := strings.Repeat("\x1b[1m中\x1b[0m", 4000)
	for _, n := range []int{-1, 0, 1, 2, 3, 40, 200} {
		for _, text := range []string{plain, styled} {
			got := screen.fit(text, n)
			if !utf8.ValidString(got) {
				t.Fatalf("fit(%d) produced invalid UTF-8", n)
			}
			if cells := screen.cells(got); cells > max(n, 0) {
				t.Fatalf("fit(%d) produced %d cells", n, cells)
			}
			if strings.Count(got, "\x1b[") != strings.Count(got, "m") {
				t.Fatalf("fit(%d) split an escape sequence: %.80q", n, got)
			}
		}
	}
}
