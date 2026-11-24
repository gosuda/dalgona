package transcript

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"slices"
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func itemOf(t *testing.T, turn, id, kind string, meta map[string]any) state.ItemView {
	t.Helper()
	metadata := map[string]jsontext.Value{}
	for name, value := range meta {
		raw, err := json.Marshal(value)
		if err != nil {
			t.Fatal(err)
		}
		metadata[name] = raw
	}
	return state.ItemView{Key: state.ItemKey{Turn: turn, Item: id}, Header: rpc.ItemHeader{ID: id, Type: kind, ItemRevision: id + "-rev", Metadata: metadata}}
}

func entriesOf(t *testing.T, turn state.TurnView) []Entry {
	t.Helper()
	return orderTurn(state.ThreadKey{Endpoint: "local", Thread: "t1"}, turn, func(it state.ItemView) facts { return readFacts(it.Header) })
}

func TestNestedCallsFollowTheirEvalRowAndOrphansKeepTheirPlace(t *testing.T) {
	done := map[string]any{"status": rpc.StatusCompleted}
	turn := state.TurnView{ID: "u1", Items: []state.ItemView{
		itemOf(t, "u1", "c1", rpc.KindDynamic, map[string]any{"tool": "eval", "status": rpc.StatusCompleted}),
		itemOf(t, "u1", "a1", rpc.KindAgent, nil),
		itemOf(t, "u1", "c1/1", rpc.KindMCP, done),
		itemOf(t, "u1", "c1/2", rpc.KindCommand, done),
		itemOf(t, "u1", "x/1", rpc.KindMCP, done),
	}}
	var got []string
	for _, e := range entriesOf(t, turn) {
		got = append(got, e.Item.Key.Item+":"+strconv.FormatBool(e.Nested))
	}
	want := "c1:false c1/1:true c1/2:true a1:false x/1:false"
	if strings.Join(got, " ") != want {
		t.Fatalf("order %v, want %s", got, want)
	}
}

func TestRowsAreChosenByItemKindNotToolName(t *testing.T) {
	cases := []struct {
		kind string
		meta map[string]any
		want Kind
	}{
		{rpc.KindMCP, map[string]any{"tool": "advisor"}, KindTool},
		{rpc.KindDynamic, map[string]any{"tool": "advisor"}, KindAdvisor},
		{rpc.KindDynamic, map[string]any{"tool": "eval"}, KindTool},
		{rpc.KindCommand, map[string]any{"tool": "advisor"}, KindCommand},
		{"webSearch", nil, KindOther},
		{rpc.KindCompaction, nil, KindCompaction},
	}
	for _, c := range cases {
		if got := readFacts(itemOf(t, "u", "i", c.kind, c.meta).Header).kind; got != c.want {
			t.Errorf("%s with %v is kind %d, want %d", c.kind, c.meta, got, c.want)
		}
	}
	if got := kindLabel("webSearch"); got != "Web search" {
		t.Fatalf("unknown kind reads %q, want the readable type name Web search", got)
	}
}

func TestVerbosityTableAndLowGrouping(t *testing.T) {
	ok := map[string]any{"status": rpc.StatusCompleted}
	failed := map[string]any{"status": rpc.StatusFailed}
	turn := state.TurnView{ID: "u1", Items: []state.ItemView{
		itemOf(t, "u1", "c1", rpc.KindCommand, ok),
		itemOf(t, "u1", "m1", rpc.KindMCP, ok),
		itemOf(t, "u1", "c2", rpc.KindCommand, failed),
		itemOf(t, "u1", "c3", rpc.KindCommand, ok),
		itemOf(t, "u1", "f1", rpc.KindFileChange, ok),
		itemOf(t, "u1", "e1", rpc.KindDynamic, map[string]any{"tool": "eval", "status": rpc.StatusCompleted}),
		itemOf(t, "u1", "a1", rpc.KindAgent, nil),
	}}
	entries := entriesOf(t, turn)
	byID := map[string]Entry{}
	for _, e := range entries {
		byID[e.Item.Key.Item] = e
	}
	expect := []struct {
		id    string
		level Level
		want  Presentation
	}{
		{"c1", LevelHigh, Presentation{Mode: ModeFull}},
		{"c2", LevelHigh, Presentation{Mode: ModeTail, Tail: 10}},
		{"c1", LevelMedium, Presentation{Mode: ModeTail, Tail: 10}},
		{"f1", LevelMedium, Presentation{Mode: ModeTail, Tail: 60}},
		{"e1", LevelMedium, Presentation{Mode: ModeLine}},
		{"m1", LevelMedium, Presentation{Mode: ModeFull}},
		{"a1", LevelLow, Presentation{Mode: ModeFull}},
		{"c2", LevelLow, Presentation{Mode: ModeTail, Tail: 10}},
	}
	for _, x := range expect {
		if got := present(byID[x.id], x.level); got != x.want {
			t.Errorf("%s at level %d presents %+v, want %+v", x.id, x.level, got, x.want)
		}
	}
	grouped := groupTools(entries)
	var shape []string
	for _, e := range grouped {
		if e.Kind == KindGroup {
			shape = append(shape, groupSummary(e.Members, func(Entry) int { return 2 }))
			continue
		}
		shape = append(shape, e.Item.Key.Item)
	}
	want := []string{"Ran 1 command · called 1 tool", "c2", "Ran 1 command · edited 2 files · called 1 tool", "a1"}
	if strings.Join(shape, "|") != strings.Join(want, "|") {
		t.Fatalf("low grouping %q, want %q", shape, want)
	}
}

func TestThinkingShowsOnlyTheLatestLineWhileStreaming(t *testing.T) {
	live := Entry{Kind: KindThinking, Item: state.ItemView{Provisional: true}}
	done := Entry{Kind: KindThinking}
	if got := present(live, LevelMedium); got != (Presentation{Mode: ModeTail, Tail: 1}) {
		t.Fatalf("streaming reasoning at medium presents %+v", got)
	}
	if got := present(done, LevelLow); got.Mode != ModeLine {
		t.Fatalf("finished reasoning at low presents %+v", got)
	}
	if got := present(live, LevelHigh); got.Mode != ModeFull {
		t.Fatalf("streaming reasoning at high presents %+v", got)
	}
}

func TestDiffCardShowsKindCountsPrefixesAndOmissions(t *testing.T) {
	var body strings.Builder
	body.WriteString("--- a/a.go\n+++ b/a.go\n@@ -1,70 +1,70 @@\n")
	for i := range 70 {
		body.WriteString("+line " + strconv.Itoa(i) + "\n")
	}
	changes := []change{
		{Path: "skip.go", Kind: rpc.KindUpdate, Omitted: true},
		{Path: "new.go", Kind: rpc.KindAdd, Diff: body.String(), Additions: 70, Counted: true, Deletions: 0, DeleteCount: true},
	}
	source, roles := presentChanges(changes, diffView{limit: 60}).source()
	lines := strings.Split(source, "\n")
	if !slices.Equal(lines[:2], []string{"diff skip.go · Changed", "Diff not shown."}) {
		t.Fatalf("omitted diff reads %q", lines[:2])
	}
	if lines[2] != "diff new.go · Added · +70 −0" {
		t.Fatalf("file heading reads %q", lines[2])
	}
	shown := lines[3 : len(lines)-1]
	if len(shown) != 60 || shown[3] != "+line 0" || [3]lineRole{roles[3], roles[5], roles[6]} != [3]lineRole{roleMeta, roleHunk, roleAdd} {
		t.Fatalf("kept %d lines starting %q with roles %v", len(shown), shown[:4], roles[3:7])
	}
	if last := lines[len(lines)-1]; last != "… 13 more lines omitted; open the full text to see them" {
		t.Fatalf("truncation note reads %q", last)
	}
	checkDiffCardTail(t, changes[1:])
}

func checkDiffCardTail(t *testing.T, changes []change) {
	t.Helper()
	tailed, _ := presentChanges(changes, diffView{limit: 60, tail: true}).source()
	tail := strings.Split(tailed, "\n")
	if tail[1] != "… 13 more lines omitted; open the full text to see them" || tail[len(tail)-1] != "+line 69" {
		t.Fatalf("failed diff tail reads %q … %q", tail[1], tail[len(tail)-1])
	}
}

func TestDiffCardIsOneFoldableSectionPerFileInFileOrder(t *testing.T) {
	changes := []change{
		{Path: "a.go", Kind: rpc.KindUpdate, Diff: "--- a/a.go\n+++ b/a.go\n@@ -1,2 +1 @@\n-old\n--- dashes are content\n+new\n"},
		{Path: "b.go", Kind: rpc.KindDelete, Omitted: true},
		{Path: "c.go", Kind: rpc.KindAdd, Diff: "--- /dev/null\n+++ b/c.go\n@@ -0,0 +1 @@\n+fresh\n"},
	}
	source, _ := presentChanges(changes, diffView{}).source()
	id := document.Identity{Endpoint: "local", Thread: "t1", Turn: "u1", Item: "f1"}
	doc, err := document.New(document.Header{Identity: id, Format: document.FormatPatch}, source)
	if err != nil {
		t.Fatal(err)
	}
	headings := []string{"diff a.go · Changed", "diff b.go · Deleted", "diff c.go · Added"}
	if doc.Len() != len(headings) {
		t.Fatalf("%d sections for %d files:\n%s", doc.Len(), len(headings), source)
	}
	checkDiffSectionFolds(t, doc, headings)
	if rows := len(doc.Block(0).Rows); rows != 7 {
		t.Fatalf("the first file section has %d rows; a deleted line starting with --- split it", rows)
	}
}

func checkDiffSectionFolds(t *testing.T, doc *document.Document, headings []string) {
	t.Helper()
	folds := doc.Folds()
	for i, want := range headings {
		b := doc.Block(i)
		first := strings.TrimSuffix(b.Projection.Text[b.Rows[0].Start:b.Rows[0].End], "\n")
		if b.Kind != document.KindDiff || first != want {
			t.Fatalf("section %d is kind %d starting %q, want a diff section starting %q", i, b.Kind, first, want)
		}
		if i >= len(folds) || folds[i].Kind != document.FoldDiff || folds[i].Blocks != (document.Span{Start: i, End: i + 1}) || folds[i].Rows != len(b.Rows) {
			t.Fatalf("section %d folds as %+v", i, folds)
		}
	}
}

func TestCommandTailShowsTenLinesAndTheTotal(t *testing.T) {
	var out strings.Builder
	for i := 1; i <= 25; i++ {
		out.WriteString("out " + strconv.Itoa(i) + "\n")
	}
	var s sheet
	tailSheet(&s, out.String(), Presentation{Mode: ModeTail, Tail: 10}, fieldText{ready: true})
	if !strings.HasPrefix(s.source, "out 16\n") || !strings.HasSuffix(s.source, "out 25") || s.note != "Showing the last 10 of 25 lines." || !s.full {
		t.Fatalf("tail %q note %q full %v", s.source, s.note, s.full)
	}
	var short sheet
	tailSheet(&short, "one\ntwo\n", Presentation{Mode: ModeTail, Tail: 10}, fieldText{ready: true})
	if short.note != "" {
		t.Fatalf("a two-line output claims truncation: %q", short.note)
	}
}

func TestLargeOutputTailCountsAllItsLines(t *testing.T) {
	tail := Presentation{Mode: ModeTail, Tail: 10}
	cases := []struct {
		name string
		ft   fieldText
		mode Presentation
		want string
	}{
		{"counting", fieldText{large: true, total: 600 << 10}, tail, "Showing the end of 600.0 KiB of output; counting its lines."},
		{"counted", fieldText{large: true, total: 600 << 10, counted: true, lines: 9600}, tail, "Showing the last 10 of 9600 lines."},
		{"capped", fieldText{large: true, total: 40 << 20, counted: true, capped: true, lines: 262144}, tail, "Showing the last 10 of more than 262144 lines."},
		{"full", fieldText{large: true, total: 600 << 10, counted: true, lines: 9600}, Presentation{Mode: ModeFull}, "Showing the end of 9600 lines (600.0 KiB) of output; open the full text for all of it."},
	}
	for _, c := range cases {
		var s sheet
		tailSheet(&s, "a\nb\nc\n", c.mode, c.ft)
		if s.note != c.want || !s.full {
			t.Errorf("%s: note %q full %v, want %q", c.name, s.note, s.full, c.want)
		}
	}
}

func TestCountedLinesMatchTheTailTotal(t *testing.T) {
	for _, text := range []string{"a\nb\n", "a\nb", "a", "a\n\n", "\n"} {
		var b body
		b.key.count = true
		b.total = uint64(len(text))
		b.countPage([]byte(text[:len(text)/2]))
		b.countPage([]byte(text[len(text)/2:]))
		if _, want := lastLines(text, 1); b.lineTotal() != want {
			t.Errorf("%q counts %d lines, the tail total is %d", text, b.lineTotal(), want)
		}
	}
}

func TestDurationsFollowTheFormattingTable(t *testing.T) {
	cases := map[int64]string{999: "999 ms", 1500: "1s", 59_999: "59s", 61_000: "1m 01s", 3_660_000: "1h 01m"}
	for ms, want := range cases {
		if got := formatDuration(ms); got != want {
			t.Errorf("%d ms formats as %q, want %q", ms, got, want)
		}
	}
}

func TestToolOutputDropsOnlyMarkerLines(t *testing.T) {
	in := "[src/a.go#1A2B]\nkept [x]\n[b.go@r0123456789abcdef0123456789abcdef.1a]\nlast"
	if got := tailored(in); got != "kept [x]\nlast" {
		t.Fatalf("tailored output %q", got)
	}
}

func TestKeywordRangesOutsideTheTextOrMidCharacterAreDropped(t *testing.T) {
	text := "한글 test"
	got := validKeywords(text, []keyword{{Word: "한글", Start: 0, End: 6}, {Word: "bad", Start: 1, End: 4}, {Word: "far", Start: 5, End: 99}, {Word: "test", Start: 7, End: 11}})
	if len(got) != 2 || got[0].Word != "한글" || got[1].Word != "test" {
		t.Fatalf("kept %+v", got)
	}
}
