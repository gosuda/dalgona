package tui

import (
	"fmt"
	"slices"
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/rpc"
)

func verbosityModel(t *testing.T, level string) model {
	t.Helper()
	m := rowModel(t, 80)
	m.screen.verbosity = level
	return holdPrints(notifyTurn(t, m, "turn/started", "inProgress"))
}

func holdPrints(m model) model {
	m.out.printing = true
	return m
}

func notifyTurn(t *testing.T, m model, method, status string) model {
	t.Helper()
	params := []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"` + status + `"}}`)
	return rawUpdate(t, m, notificationMsg{method: method, params: params})
}

func numberedLines(n int) string {
	var b strings.Builder
	for i := 1; i <= n; i++ {
		fmt.Fprintf(&b, "line %d\n", i)
	}
	return b.String()
}

func commandItem(id, status string, lines int) string {
	return fmt.Sprintf(`{"type":"commandExecution","id":%q,"command":"seq %d","cwd":"/work","status":%q,"aggregatedOutput":%s,"exitCode":0}`, id, lines, status, strconv.Quote(numberedLines(lines)))
}

func entryKeys(m model) []string {
	keys := make([]string, 0, len(m.out.entries))
	for _, entry := range m.out.entries {
		keys = append(keys, entry.key)
	}
	return keys
}

func TestVerbosityLevels(t *testing.T) {
	t.Parallel()
	for _, level := range []string{"high", "medium", "low"} {
		m := verbosityModel(t, level)
		m = notifyItem(t, m, "item/completed", commandItem("c1", "completed", 30))
		if level == "medium" {
			m = thinkingRow(t, m)
		}
		assertLevelRows(t, level, strings.Join(m.out.pending, "\n"))
	}
}

func assertLevelRows(t *testing.T, level, text string) {
	t.Helper()
	if level == "low" {
		if strings.Contains(text, "line ") {
			t.Fatalf("low printed command output: %q", text)
		}
		return
	}
	want := 30
	if level == "medium" {
		want = 10
	}
	if got := strings.Count(text, "line "); got != want {
		t.Fatalf("%s prints %d output lines, want %d: %q", level, got, want, text)
	}
	if level == "medium" && !strings.Contains(text, "… 20 earlier lines") {
		t.Fatalf("medium output has no earlier-lines marker: %q", text)
	}
}

func thinkingRow(t *testing.T, m model) model {
	t.Helper()
	for _, delta := range []string{"a\n", "b\n", "c\n"} {
		params := []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"r1","delta":` + strconv.Quote(delta) + `}`)
		m = rawUpdate(t, m, notificationMsg{method: "item/reasoning/textDelta", params: params})
	}
	m = notifyItem(t, m, "item/completed", `{"type":"reasoning","id":"r1","status":"completed"}`)
	assertContains(t, stripANSI(strings.Join(m.out.pending, "\n")), "Thinking · 3 lines · ctrl+o")
	return m
}

func TestVerbosityLowSummarisesGroupAtTurnEnd(t *testing.T) {
	t.Parallel()
	m := verbosityModel(t, "low")
	m = notifyItem(t, m, "item/completed", commandItem("c1", "completed", 3))
	m = notifyItem(t, m, "item/completed", commandItem("e1/1", "completed", 3))
	m = notifyItem(t, m, "item/completed", `{"type":"fileChange","id":"f1","status":"completed","changes":[{"path":"a.go","kind":{"type":"update"},"diff":"","additions":1,"deletions":1},{"path":"b.go","kind":{"type":"add"},"diff":"","additions":2,"deletions":0}]}`)
	m = notifyItem(t, m, "item/completed", `{"type":"dynamicToolCall","id":"d1","tool":"read","arguments":{"path":"a.go"},"status":"completed"}`)
	m = notifyItem(t, m, "item/completed", `{"type":"dynamicToolCall","id":"e1","tool":"eval","arguments":{"code":"1"},"status":"completed"}`)
	m = notifyItem(t, m, "item/completed", `{"type":"dynamicToolCall","id":"x1","tool":"todo","arguments":{"items":[]},"status":"completed"}`)
	if len(m.out.pending) != 0 {
		t.Fatalf("low printed tool rows before the turn ended: %q", m.out.pending)
	}
	m = notifyTurn(t, m, "turn/completed", "completed")
	assertContains(t, entryText(t, m, "group:turn-1:1"), "Ran 2 commands · edited 2 files · called 2 tools")
}

func TestFailedItemFlushesGroupFirst(t *testing.T) {
	t.Parallel()
	m := verbosityModel(t, "low")
	m = notifyItem(t, m, "item/completed", commandItem("c1", "completed", 3))
	m = notifyItem(t, m, "item/completed", commandItem("c2", "failed", 30))
	keys := entryKeys(m)
	if len(keys) != 3 || keys[1] != "group:turn-1:1" || keys[2] != fixtureItemKey("c2") {
		t.Fatalf("entry order %q, want the group line before the failed row", keys)
	}
	text := strings.Join(m.out.pending, "\n")
	if got := strings.Count(text, "line "); got != 10 {
		t.Fatalf("failed item prints %d output lines at low, want 10: %q", got, text)
	}
	assertContains(t, text, "… 20 earlier lines", "Ran 1 command")
}

func TestAnswerFlushesGroupFirst(t *testing.T) {
	t.Parallel()
	m := verbosityModel(t, "low")
	m = notifyItem(t, m, "item/completed", commandItem("c1", "completed", 3))
	m = notifyItem(t, m, "item/completed", `{"type":"agentMessage","id":"a1","text":"Answer."}`)
	keys := entryKeys(m)
	if len(keys) != 3 || keys[1] != "group:turn-1:1" || keys[2] != "block:t1:a1:0" {
		t.Fatalf("entry order %q, want the group line before the answer", keys)
	}
}

func TestGroupFlushesBeforeThreadSwitch(t *testing.T) {
	t.Parallel()
	m := verbosityModel(t, "low")
	m = notifyItem(t, m, "item/completed", commandItem("c1", "completed", 3))
	m = notifyItem(t, m, "item/completed", commandItem("c2", "completed", 3))
	m, _ = drive(t, m, resultMsg{op: "resumed", value: &resumedThread{Thread: rpc.Thread{ID: "t2", Name: "other"}}})
	keys := entryKeys(m)
	group := slices.Index(keys, "group:turn-1:1")
	header := slices.IndexFunc(keys, func(key string) bool { return strings.HasPrefix(key, "thread:t2:") })
	if group < 0 || header < 0 || group > header {
		t.Fatalf("entry order %q, want the open group printed before thread t2's header", keys)
	}
	assertContains(t, entryText(t, m, "group:turn-1:1"), "Ran 2 commands")
}

func TestReasoningTailThenOneRow(t *testing.T) {
	t.Parallel()
	m := verbosityModel(t, "medium")
	for _, delta := range []string{"a\n", "b\n", "c\n"} {
		params := []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"r1","delta":` + strconv.Quote(delta) + `}`)
		m = rawUpdate(t, m, notificationMsg{method: "item/reasoning/textDelta", params: params})
		if len(m.out.pending) != 0 || len(m.out.entries) != 0 {
			t.Fatalf("a delta printed before the item completed: pending %q, entries %d", m.out.pending, len(m.out.entries))
		}
	}
	if got := m.activity(); got != "c" {
		t.Fatalf("live row = %q, want c", got)
	}
	m = notifyItem(t, m, "item/completed", `{"type":"reasoning","id":"r1","status":"completed"}`)
	assertContains(t, stripANSI(strings.Join(m.out.pending, "\n")), "Thinking · 3 lines · ctrl+o")
}

func TestDiffKeepsSixtyLinesAtMedium(t *testing.T) {
	t.Parallel()
	var diff strings.Builder
	for i := 1; i <= 70; i++ {
		fmt.Fprintf(&diff, "+line %d\n", i)
	}
	item := fmt.Sprintf(`{"type":"fileChange","id":"f1","status":"completed","changes":[{"path":"a.go","kind":{"type":"update"},"diff":%s,"additions":70,"deletions":0}]}`, strconv.Quote(diff.String()))
	medium := notifyItem(t, holdPrints(rowModel(t, 80)), "item/completed", item)
	assertContains(t, stripANSI(strings.Join(medium.out.pending, "\n")), "… 10 more lines · ctrl+o")
	high := rowModel(t, 80)
	high.screen.verbosity = "high"
	full := notifyItem(t, high, "item/completed", item)
	if strings.Contains(entryText(t, full, fixtureItemKey("f1")), "more lines") {
		t.Fatalf("high verbosity cut the diff")
	}
}

func TestEvalRowAtHighShowsCodeLine(t *testing.T) {
	t.Parallel()
	item := `{"type":"dynamicToolCall","id":"e1","tool":"eval","arguments":{"code":"tools.read(path = 'a')\nprint(1)"},"status":"completed","aggregatedOutput":"ok"}`
	high := rowModel(t, 100)
	high.screen.verbosity = "high"
	assertContains(t, entryText(t, notifyItem(t, high, "item/completed", item), fixtureItemKey("e1")), "Eval", "tools.read(path = 'a')", "ok")
	medium := notifyItem(t, holdPrints(rowModel(t, 100)), "item/completed", item)
	assertContains(t, stripANSI(strings.Join(medium.out.pending, "\n")), "Eval · 2 lines of code · Done")
}

func TestVerbositySettingIsApplied(t *testing.T) {
	t.Parallel()
	m := rowModel(t, 80)
	m.applySetting(settingValue{Key: "verbosity", Value: "high"})
	if m.screen.verbosity != "high" {
		t.Fatalf("verbosity = %q, want high", m.screen.verbosity)
	}
}

func TestFailedFileChangeKeepsLastTenLines(t *testing.T) {
	t.Parallel()
	var diff strings.Builder
	for i := 1; i <= 100; i++ {
		fmt.Fprintf(&diff, "+line %d\n", i)
	}
	item := fmt.Sprintf(`{"type":"fileChange","id":"f1","status":"failed","changes":[{"path":"a.go","kind":{"type":"update"},"diff":%s,"additions":100,"deletions":0}]}`, strconv.Quote(diff.String()))
	m := notifyItem(t, holdPrints(rowModel(t, 80)), "item/completed", item)
	text := stripANSI(strings.Join(m.out.pending, "\n"))
	assertContains(t, text, "+line 100", "… 90 earlier lines")
	if strings.Contains(text, "+line 52") {
		t.Fatalf("failed diff printed the 60-line preview, not its last 10 lines:\n%s", text)
	}
}

func TestDeclinedCommandAtLowPrintsOwnRow(t *testing.T) {
	t.Parallel()
	m := verbosityModel(t, "low")
	m = notifyItem(t, m, "item/completed", commandItem("c1", "completed", 3))
	m = notifyItem(t, m, "item/completed", commandItem("c2", "declined", 3))
	text := stripANSI(strings.Join(m.out.pending, "\n"))
	assertContains(t, text, "Ran 1 command", "Declined")
	assertContains(t, entryText(t, m, fixtureItemKey("c2")), "Declined")
}

func TestReviewEntryKeepsFullTextAtEveryLevel(t *testing.T) {
	t.Parallel()
	medium := notifyItem(t, verbosityModel(t, "medium"), "item/completed", commandItem("c1", "completed", 30))
	assertContains(t, entryText(t, medium, fixtureItemKey("c1")), "line 15", "line 30")
	if strings.Contains(strings.Join(medium.out.pending, "\n"), "line 15") {
		t.Fatalf("medium printed the full output instead of its last 10 lines")
	}
	low := notifyItem(t, verbosityModel(t, "low"), "item/completed", commandItem("c1", "completed", 30))
	assertContains(t, entryText(t, low, fixtureItemKey("c1")), "line 15", "line 30")
	evalItem := `{"type":"dynamicToolCall","id":"e1","tool":"eval","arguments":{"code":"tools.read(path = 'a')\nprint(1)"},"status":"completed","aggregatedOutput":` + strconv.Quote(numberedLines(15)) + `}`
	evalMedium := notifyItem(t, verbosityModel(t, "medium"), "item/completed", evalItem)
	assertContains(t, entryText(t, evalMedium, fixtureItemKey("e1")), "tools.read(path = 'a')", "line 15")
}

func TestFailedItemKeepsLastLinesAtLow(t *testing.T) {
	t.Parallel()
	for _, level := range []string{"medium", "low"} {
		m := notifyItem(t, verbosityModel(t, level), "item/completed", commandItem("c1", "failed", 30))
		text := stripANSI(strings.Join(m.out.pending, "\n"))
		assertContains(t, text, "… 20 earlier lines", "line 30", "Failed")
		if got := strings.Count(text, "line "); got != 10 {
			t.Fatalf("failed item prints %d output lines at %s, want 10: %q", got, level, text)
		}
	}
}
