package tui

import (
	json "encoding/json/v2"
	"fmt"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

var rowSGR = regexp.MustCompile(`\x1b\[([0-9;]*)m`)

func settledText(m *model, item rpc.Item) string {
	return strings.Join(m.renderItem(item, verbosityMedium), "\n")
}

func hasBackground(text string) bool {
	for _, match := range rowSGR.FindAllStringSubmatch(text, -1) {
		if backgroundParams(strings.Split(match[1], ";")) {
			return true
		}
	}
	return false
}

func backgroundParams(params []string) bool {
	for i := 0; i < len(params); i++ {
		code, _ := strconv.Atoi(params[i])
		if backgroundCode(code) {
			return true
		}
		if code == 38 && i+1 < len(params) && params[i+1] == "2" {
			i += 4
		}
	}
	return false
}

func backgroundCode(code int) bool {
	return code == 48 || (code >= 40 && code <= 47) || (code >= 100 && code <= 107)
}

func rowModel(t *testing.T, width int) model {
	t.Helper()
	m := newModel(nil, Options{}, nil)
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: width, Height: 24})
	m = settleFrames(m)
	m = rawUpdate(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)})
	return settleFrames(m)
}

func notifyItem(t *testing.T, m model, method, item string) model {
	t.Helper()
	params := []byte(`{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":` + item + `}`)
	return rawUpdate(t, m, notificationMsg{method: method, params: params})
}

func fixtureItemKey(id string) string {
	return itemKey("t1", "turn-1", id)
}

func entryText(t *testing.T, m model, key string) string {
	t.Helper()
	for _, entry := range m.out.entries {
		if entry.key == key {
			return stripANSI(entry.text)
		}
	}
	t.Fatalf("no row committed under %q", key)
	return ""
}

func TestCommittedRowPrintedOnce(t *testing.T) {
	t.Parallel()
	m := rowModel(t, 80)
	completed := notificationMsg{method: "item/completed", params: []byte(`{"threadId":"t1","turnId":"turn-1","completedAtMs":3,"item":{"type":"commandExecution","id":"i1","command":"go vet ./...","cwd":"/work","status":"completed","aggregatedOutput":"","exitCode":0}}`)}
	m = rawUpdate(t, m, completed)
	m = settleFrames(m)
	m = rawUpdate(t, m, completed)
	if len(m.out.pending) != 0 || m.out.printing {
		t.Fatalf("second completion queued rows again: pending %q printing %v", m.out.pending, m.out.printing)
	}
}

func TestCommitSplitsByHeight(t *testing.T) {
	t.Parallel()
	m := rowModel(t, 40)
	m.screen.height = 8
	m.screen.verbosity = "high"
	lines := make([]string, 30)
	for i := range lines {
		lines[i] = "line " + strconv.Itoa(i+1)
	}
	params, err := json.Marshal(map[string]any{
		"threadId": "t1", "turnId": "turn-1", "completedAtMs": 3,
		"item": map[string]any{"type": "commandExecution", "id": "i2", "command": "seq 30", "status": "completed", "aggregatedOutput": strings.Join(lines, "\n"), "exitCode": 0},
	})
	if err != nil {
		t.Fatal(err)
	}
	m = rawUpdate(t, m, notificationMsg{method: "item/completed", params: params})
	bound := m.screen.height - len(m.dockLines())
	if bound != 5 {
		t.Fatalf("bound = %d, want 5 for an 8-row window with a 3-row dock", bound)
	}
	want := m.out.entries[len(m.out.entries)-1].text
	var got []string
	for len(m.out.pending) > 0 {
		chunk := m.takeChunk(bound)
		if len(chunk) == 0 || len(chunk) > bound {
			t.Fatalf("chunk has %d rows, want 1 to %d", len(chunk), bound)
		}
		got = append(got, chunk...)
	}
	if strings.Join(got, "\n") != want {
		t.Fatalf("chunks lost or reordered rows:\n%q\nwant\n%q", strings.Join(got, "\n"), want)
	}
}

type rowStateCase struct {
	name    string
	item    rpc.Item
	columns []string
}

func rowStateCases() []rowStateCase {
	statuses := []struct{ status, word string }{{"completed", "Done"}, {"failed", "Failed"}, {"declined", "Declined"}, {"inProgress", "Working"}}
	var cases []rowStateCase
	for _, s := range statuses {
		command := rpc.Item{Type: "commandExecution", Command: strings.Repeat("a", 200), Status: s.status}
		change := rpc.Item{Type: "fileChange", Status: s.status, Changes: []rpc.FileChange{{Path: "a.go", Kind: "update", Additions: 1, Deletions: 1}}}
		cases = append(cases,
			rowStateCase{"command " + s.word, command, []string{"$", s.word, "a"}},
			rowStateCase{"file change " + s.word, change, []string{s.word, "Edited", "a.go"}},
		)
	}
	return cases
}

func assertRowSeparatesColumns(t *testing.T, m *model, tc rowStateCase, width int) {
	t.Helper()
	lines := m.renderItem(tc.item, verbosityMedium)
	for _, line := range lines {
		if cells := m.screen.cells(line); cells > width-1 {
			t.Fatalf("%s at %d columns is %d cells, want at most %d: %q", tc.name, width, cells, width-1, line)
		}
	}
	header := stripANSI(lines[0])
	fields := strings.Fields(header)
	if len(fields) < len(tc.columns) {
		t.Fatalf("%s at %d columns has %d fields in %q, want at least %d", tc.name, width, len(fields), header, len(tc.columns))
	}
	last := len(tc.columns) - 1
	if !slices.Equal(fields[:last], tc.columns[:last]) || !strings.HasPrefix(fields[last], tc.columns[last]) {
		t.Fatalf("%s at %d columns reads %q, want the fields %q kept apart", tc.name, width, header, tc.columns)
	}
}

func TestRowFitsOneCellShort(t *testing.T) {
	t.Parallel()
	for _, width := range []int{40, 100} {
		m := newModel(nil, Options{}, nil)
		m.screen.width = width
		for _, tc := range rowStateCases() {
			assertRowSeparatesColumns(t, &m, tc, width)
		}
	}
}

func TestFileChangeRowShowsPathsAndDiff(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 100
	item := rpc.Item{Type: "fileChange", Status: "completed", Changes: []rpc.FileChange{{Path: "a.go", Kind: "update", Diff: "@@\n-old\n+new", Additions: 1, Deletions: 1}}}
	assertContains(t, stripANSI(settledText(&m, item)), "Edited a.go", "+1 −1", "-old", "+new")
}

func TestFailedRowIsBoldTextNotFill(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 100
	m.screen.theme = newTheme(themeNight, true)
	row := m.renderItem(rpc.Item{Type: "commandExecution", Command: "go test", Status: "failed"}, verbosityMedium)[0]
	if !strings.Contains(row, "\x1b[1;38;2;232;112;95mFailed") {
		t.Fatalf("failed word is not bold failed-text: %q", row)
	}
	if hasBackground(row) {
		t.Fatalf("failed row paints a background: %q", row)
	}
}

func TestNestedReadRowKeepsSlashID(t *testing.T) {
	t.Parallel()
	m := rowModel(t, 100)
	m = notifyItem(t, m, "item/started", `{"type":"dynamicToolCall","id":"e1","tool":"eval","arguments":{"code":"tools.read(path = 'a.go')"},"status":"inProgress"}`)
	m = notifyItem(t, m, "item/started", `{"type":"dynamicToolCall","id":"e1/1","tool":"read","arguments":{"path":"a.go"},"status":"inProgress"}`)
	m = notifyItem(t, m, "item/completed", `{"type":"dynamicToolCall","id":"e1/1","tool":"read","arguments":{"path":"a.go"},"status":"completed"}`)
	assertContains(t, entryText(t, m, fixtureItemKey("e1/1")), "Read a.go")
}

func TestNestedWebFetchRowShowsURL(t *testing.T) {
	t.Parallel()
	m := rowModel(t, 100)
	m = notifyItem(t, m, "item/completed", `{"type":"dynamicToolCall","id":"e1/4","tool":"web_fetch","arguments":{"url":"https://example.com"},"status":"completed"}`)
	assertContains(t, entryText(t, m, fixtureItemKey("e1/4")), "Web_fetch", "https://example.com")
}

func TestNestedExecRowIsCommand(t *testing.T) {
	t.Parallel()
	m := rowModel(t, 100)
	m = notifyItem(t, m, "item/completed", `{"type":"commandExecution","id":"e1/6","command":"go test ./...","status":"completed","aggregatedOutput":"","exitCode":0}`)
	text := entryText(t, m, fixtureItemKey("e1/6"))
	if !strings.HasPrefix(text, "$") {
		t.Fatalf("nested exec row does not start with $: %q", text)
	}
	assertContains(t, text, "go test ./...")
}

func TestNestedPatchRowIsEdit(t *testing.T) {
	t.Parallel()
	m := rowModel(t, 100)
	m = notifyItem(t, m, "item/completed", `{"type":"fileChange","id":"e1/7","status":"completed","changes":[{"path":"a.go","kind":{"type":"update"},"diff":"@@\n-old\n+new","additions":1,"deletions":1}]}`)
	assertContains(t, entryText(t, m, fixtureItemKey("e1/7")), "Edited a.go", "+1 −1", "-old", "+new")
}

func TestEvalRowAtMediumIsOneLine(t *testing.T) {
	t.Parallel()
	m := holdPrints(rowModel(t, 100))
	m = notifyItem(t, m, "item/completed", `{"type":"dynamicToolCall","id":"e1","tool":"eval","arguments":{"code":"a\nb\nc"},"status":"completed"}`)
	text := strings.TrimSpace(stripANSI(strings.Join(m.out.pending, "\n")))
	if text != "Eval · 3 lines of code · Done" {
		t.Fatalf("eval row = %q, want one line", text)
	}
}

func TestNestedTodoWritesNoToolRow(t *testing.T) {
	t.Parallel()
	m := rowModel(t, 100)
	m = notifyItem(t, m, "item/completed", `{"type":"dynamicToolCall","id":"e1/2","tool":"todo","arguments":{"items":[]},"status":"completed"}`)
	for _, entry := range m.out.entries {
		if entry.key == fixtureItemKey("e1/2") && entry.text != "" {
			t.Fatalf("successful todo wrote a tool row: %q", entry.text)
		}
	}
}

func TestFailedEvalKeepsLastTenOutputLines(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 100
	var output []string
	for i := 1; i <= 12; i++ {
		output = append(output, fmt.Sprintf("out %02d", i))
	}
	item := rpc.Item{Type: "dynamicToolCall", Tool: "eval", Status: "failed", Arguments: []byte(`{"code":"tools.read(path = 'x')"}`), Output: strings.Join(output, "\n")}
	text := stripANSI(settledText(&m, item))
	assertContains(t, text, "Eval · 1 line of code · Failed", "… 2 earlier lines", "out 03", "out 12")
	if strings.Contains(text, "out 02") {
		t.Fatalf("failed eval kept more than its last 10 output lines:\n%s", text)
	}
}

func TestEvalWithUndecodableArgumentsOmitsCount(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 100
	item := rpc.Item{Type: "dynamicToolCall", Tool: "eval", Status: "completed", Arguments: []byte(`[1]`)}
	text := strings.TrimRight(stripANSI(settledText(&m, item)), "\n")
	if text != "Eval · Done" {
		t.Fatalf("eval row = %q, want the count omitted", text)
	}
}

func TestDiffDashLineInsideHunkIsRemovedLine(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 100
	m.screen.theme = newTheme(themeNight, true)
	diff := "--- a/q.sql\n+++ b/q.sql\n@@ -1,2 +1,2 @@\n--- note\n+++ x\n"
	item := rpc.Item{Type: "fileChange", Status: "completed", Changes: []rpc.FileChange{{Path: "q.sql", Kind: "update", Diff: diff, Additions: 1, Deletions: 1}}}
	row := settledText(&m, item)
	assertContains(t, row, m.screen.theme.paint(roleDim, "--- a/q.sql"), m.screen.theme.paint(roleFailedText, "--- note"), m.screen.theme.paint(roleAddedText, "+++ x"))
}

func TestEmptyAgentMessageAndNilCommitPrintNothing(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 100
	if got := m.renderItem(rpc.Item{Type: "agentMessage", Text: ""}, verbosityMedium); len(got) != 0 {
		t.Fatalf("empty agent message rendered %q, want no rows", got)
	}
	m.commit(fixtureItemKey("empty"), nil)
	if len(m.out.entries) != 0 || len(m.out.pending) != 0 {
		t.Fatalf("empty commit recorded entries %d and pending %q", len(m.out.entries), m.out.pending)
	}
}

func leadingSpaces(line string) int {
	return len(line) - len(strings.TrimLeft(line, " "))
}

func TestBodyLinesStartUnderSubject(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 60
	cases := []struct {
		name    string
		item    rpc.Item
		subject string
		body    string
	}{
		{"unlabelled command", rpc.Item{Type: "commandExecution", Command: "ls", Status: "completed", Output: "a.txt"}, "ls", "a.txt"},
		{"labelled file change", rpc.Item{Type: "fileChange", Status: "completed", Changes: []rpc.FileChange{{Path: "a.go", Kind: "update", Diff: "@@\n+new", Additions: 1}}}, "a.go", "+new"},
	}
	for _, tc := range cases {
		lines := strings.Split(stripANSI(settledText(&m, tc.item)), "\n")
		column := strings.Index(lines[0], tc.subject)
		var body string
		for _, line := range lines {
			if strings.TrimSpace(line) == tc.body {
				body = line
			}
		}
		if body == "" {
			t.Fatalf("%s: no body line %q in %q", tc.name, tc.body, lines)
		}
		if got := leadingSpaces(body); got != column {
			t.Fatalf("%s: body starts at column %d, subject at column %d:\n%s", tc.name, got, column, strings.Join(lines, "\n"))
		}
	}
}

func TestDynamicAndMCPRowsShowDuration(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 60
	twoSeconds, threeSeconds := int64(2000), int64(3000)
	cases := []struct {
		name string
		item rpc.Item
		want string
	}{
		{"dynamic tool call", rpc.Item{Type: "dynamicToolCall", Tool: "read", Arguments: []byte(`{"path":"a.go"}`), Status: "completed", DurationMs: &twoSeconds}, "2s"},
		{"mcp tool call", rpc.Item{Type: "mcpToolCall", Server: "docs", Tool: "lookup", Status: "completed", DurationMs: &threeSeconds}, "3s"},
	}
	for _, tc := range cases {
		header, _, _ := strings.Cut(stripANSI(settledText(&m, tc.item)), "\n")
		if !strings.HasSuffix(strings.TrimRight(header, " "), tc.want) {
			t.Fatalf("%s: row %q does not end with duration %q", tc.name, header, tc.want)
		}
	}
}
