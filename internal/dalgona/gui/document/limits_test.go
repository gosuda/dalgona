package document

import (
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
	"github.com/yuin/goldmark/ast"
	"github.com/yuin/goldmark/text"
)

func diagnosticFor(d *Document, limit Limit) (Diagnostic, bool) {
	for _, diagnostic := range d.Diagnostics() {
		if diagnostic.Limit == limit {
			return diagnostic, true
		}
	}
	return Diagnostic{}, false
}

func wholeText(t *testing.T, d *Document) string {
	t.Helper()
	start, _ := d.Start()
	end, _ := d.End()
	text, err := d.Text(start, end)
	if err != nil {
		t.Fatal(err)
	}
	return text
}

func TestDeepBlockNestingStopsAtTheDeclaredDepth(t *testing.T) {
	for _, tc := range []struct {
		marker string
		text   string
	}{
		{marker: "> ", text: "deep"},
		{marker: "- ", text: "item"},
	} {
		t.Run(tc.text, func(t *testing.T) {
			checkCappedNesting(t, tc.marker, tc.text)
		})
	}
}

func checkCappedNesting(t *testing.T, marker, projected string) {
	t.Helper()
	source := strings.Repeat(marker, 20_000) + projected
	began := time.Now()
	d := markdown(t, projected, source)
	if elapsed := time.Since(began); elapsed > 20*time.Second {
		t.Fatalf("deep nesting took %v", elapsed)
	}
	diagnostic, ok := diagnosticFor(d, LimitNesting)
	if !ok || !strings.Contains(diagnostic.Message, "32 levels") {
		t.Fatalf("diagnostics %+v", d.Diagnostics())
	}
	if d.Len() != 1 {
		t.Fatalf("block count %d", d.Len())
	}
	block := d.Block(0)
	if len(block.Containers) != MaxNesting || block.Projection.Text != projected {
		t.Fatalf("deep block %d containers, text %q", len(block.Containers), block.Projection.Text)
	}
}

func TestGoldmarkOpensAtMostOneContainerPastTheLimit(t *testing.T) {
	for _, marker := range []string{"> ", "- ", "1) ", "> - "} {
		source := strings.Repeat(marker, MaxNesting+3) + "text"
		root := markdownParser.Parse(text.NewReader([]byte(source)))
		depth := 0
		err := ast.Walk(root, func(node ast.Node, entering bool) (ast.WalkStatus, error) {
			switch node.Kind() {
			case ast.KindBlockquote, ast.KindListItem:
				if entering {
					depth++
				} else {
					depth--
				}
			}
			if depth > MaxNesting+1 {
				t.Fatalf("%q opened %d container levels", marker, depth)
			}
			return ast.WalkContinue, nil
		})
		if err != nil {
			t.Fatal(err)
		}
	}
}

func TestCappedContainersKeepRawContentAndSourceOffsets(t *testing.T) {
	prefix := strings.Repeat("> ", MaxNesting+10)
	less := prefix[:len(prefix)-2]
	source := prefix + "```\n" + prefix + "> - raw\n" + prefix + "```\n\n" +
		prefix + "first\n" + less + "lazy\n" + prefix + "last"
	d := markdown(t, "capped", source)
	if d.Source() != source || d.Len() != 2 {
		t.Fatalf("source changed or block count %d is not 2", d.Len())
	}
	if got := d.Block(0).Projection.Text; got != "> - raw\n" {
		t.Fatalf("code = %q", got)
	}
	if got := d.Block(1).Projection.Text; got != "first lazy last" {
		t.Fatalf("paragraph = %q", got)
	}
	for _, word := range []string{"raw", "first", "lazy", "last"} {
		from := strings.Index(source, word)
		for offset := from; offset < from+len(word); offset++ {
			position, err := d.Position(offset, After)
			if err != nil || position.Offset != offset {
				t.Fatalf("%s at byte %d maps to %+v: %v", word, offset, position, err)
			}
		}
	}
}

func TestCappedEmptyListKeepsItsSourceLine(t *testing.T) {
	prefix := strings.Repeat("> ", MaxNesting+1)
	first := prefix + "> - first\n"
	source := first + prefix + "-\n" + prefix + "  \n" + prefix + "  - after\n"
	d := markdown(t, "capped-empty", source)
	if d.Len() != 3 {
		t.Fatalf("block count %d", d.Len())
	}
	if empty := d.Block(1); empty.Span.Start != len(first) || empty.Projection.Text != "" {
		t.Fatalf("empty list item at %+v with text %q", empty.Span, empty.Projection.Text)
	}
	if got := d.Block(2).Projection.Text; got != "after" {
		t.Fatalf("following list item text %q", got)
	}
}

func TestCappedTasksUseTheOriginalListScope(t *testing.T) {
	for _, tc := range []struct {
		prefix string
		task   Task
		text   string
	}{
		{prefix: strings.Repeat("> ", MaxNesting+1) + "- ", task: TaskDone, text: "item"},
		{prefix: strings.Repeat("- ", MaxNesting+1) + "> ", task: TaskNone, text: "[x] item"},
	} {
		source := tc.prefix + "[x] item"
		d := markdown(t, "capped-task", source)
		if d.Len() != 1 {
			t.Fatalf("block count %d", d.Len())
		}
		block := d.Block(0)
		if block.Task != tc.task || block.Projection.Text != tc.text {
			t.Fatalf("task state %v and text %q, want %v and %q", block.Task, block.Projection.Text, tc.task, tc.text)
		}
	}
}

func TestDeepInlineNestingShowsLiteralSource(t *testing.T) {
	line := strings.Repeat("*a ", 40) + "x" + strings.Repeat(" b*", 40)
	d := markdown(t, "inline", line)
	if _, ok := diagnosticFor(d, LimitNesting); !ok {
		t.Fatalf("diagnostics %+v", d.Diagnostics())
	}
	if got := d.Block(0).Projection.Text; got != line {
		t.Fatalf("deep inline block shows %q", got)
	}
	shallow := markdown(t, "shallow", strings.Repeat("*a ", 10)+"x"+strings.Repeat(" b*", 10))
	if _, ok := diagnosticFor(shallow, LimitNesting); ok {
		t.Fatalf("shallow emphasis reported %+v", shallow.Diagnostics())
	}
}

func TestTenMegabyteLineHitsTheSourceLimitsAndKeepsEveryByte(t *testing.T) {
	line := strings.Repeat("가나다라 abc ", 10<<20/17+1)
	source := "intro\n\n" + line + "\n\nafter"
	began := time.Now()
	d := markdown(t, "huge", source)
	if elapsed := time.Since(began); elapsed > 30*time.Second {
		t.Fatalf("10 MB line took %v", elapsed)
	}
	checkHugeDiagnostics(t, d)
	checkHugeRowsCapped(t, d, source)
	checkLongLineDiagnostic(t)
}

func checkHugeDiagnostics(t *testing.T, d *Document) {
	t.Helper()
	if _, ok := diagnosticFor(d, LimitMarkdownBytes); !ok {
		t.Fatalf("diagnostics %+v", d.Diagnostics())
	}
	if _, ok := diagnosticFor(d, LimitRow); !ok {
		t.Fatalf("row limit diagnostic missing: %+v", d.Diagnostics())
	}
}

func checkHugeRowsCapped(t *testing.T, d *Document, source string) {
	t.Helper()
	if got := wholeText(t, d); got != source {
		t.Fatal("literal presentation changed the source text")
	}
	for i := range d.Len() {
		checkCappedRow(t, d.Block(i))
	}
}

func checkCappedRow(t *testing.T, b Block) {
	t.Helper()
	for _, row := range b.Rows {
		if row.Len() > MaxRowBytes {
			t.Fatalf("row %v exceeds %d bytes", row, MaxRowBytes)
		}
	}
	checkRows(t, b)
}

func checkLongLineDiagnostic(t *testing.T) {
	t.Helper()
	long := markdown(t, "line", "short\n\n"+strings.Repeat("x", MaxMarkdownLineBytes+1)+"\n")
	if diagnostic, ok := diagnosticFor(long, LimitMarkdownLine); !ok || diagnostic.Span.Start != len("short\n\n") {
		t.Fatalf("line diagnostics %+v", long.Diagnostics())
	}
}

func TestStreamingPastTheLimitSwitchesToLiteralSource(t *testing.T) {
	d := openMarkdown(t, "grow", "# Title\n\n")
	d = appendAll(t, d, strings.Repeat("y", MaxMarkdownLineBytes), "z\n")
	if _, ok := diagnosticFor(d, LimitMarkdownLine); !ok {
		t.Fatalf("diagnostics %+v", d.Diagnostics())
	}
	if got := wholeText(t, d); got != d.Source() {
		t.Fatal("literal stream changed the source text")
	}
	d = appendAll(t, d, "more\n")
	if got := wholeText(t, d); got != d.Source() {
		t.Fatal("literal append changed the source text")
	}
}

func TestCodeOutputSplitsLongLinesAtGraphemes(t *testing.T) {
	line := strings.Repeat("e\u0301👩\u200d👩\u200d👧", MaxRowBytes/10)
	d, err := New(Header{Identity: committed(t, "out"), Format: FormatCode}, line+"\nend\n")
	if err != nil {
		t.Fatal(err)
	}
	b := d.Block(0)
	if len(b.Rows) < 3 {
		t.Fatalf("rows %v", b.Rows)
	}
	for _, row := range b.Rows[:len(b.Rows)-1] {
		if row.Len() > MaxRowBytes || inputtext.CheckRange(b.Projection.Text, inputtext.Range{Start: row.Start, End: row.End}) != nil {
			t.Fatalf("row %v splits a grapheme or exceeds the limit", row)
		}
	}
	if _, ok := diagnosticFor(d, LimitRow); !ok {
		t.Fatalf("diagnostics %+v", d.Diagnostics())
	}
}
