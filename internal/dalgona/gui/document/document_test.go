package document

import (
	"errors"
	"html"
	"math/rand/v2"
	"strings"
	"testing"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

var markdownWords = []string{
	"alpha", "한국어", "日本語", "👩\u200d👩\u200d👧", "🇰🇷🇯🇵", "e\u0301", "مرحبا", "fi", "\u200b",
	"*em*", "**strong**", "~~del~~", "`co de`", "``a`b``", "[link](https://x.y \"t\")", "[ref]", "<https://a.b>",
	"www.example.com", "\\*", "\\|", "&amp;", "&#x1F600;", "&#0;", "&nosuch;", "$x^2$", "\\(y\\)", "$5",
	"![alt *t*](u)", "a\tb", "<span>", "_u_", "[x](<a b>)", "&copy;", "\\\\",
}

func randomInline(r *rand.Rand, words int) string {
	parts := make([]string, words)
	for i := range parts {
		parts[i] = markdownWords[r.IntN(len(markdownWords))]
	}
	return strings.Join(parts, " ")
}

func randomBlock(r *rand.Rand, eol string) string {
	choice := r.IntN(12)
	if choice < 6 {
		return randomBlockHead(r, eol, choice)
	}
	return randomBlockTail(r, eol, choice)
}

func randomBlockHead(r *rand.Rand, eol string, choice int) string {
	switch choice {
	case 0:
		return strings.Repeat("#", 1+r.IntN(6)) + " " + randomInline(r, 3) + eol
	case 1:
		return "- " + randomInline(r, 3) + eol + "  - [x] " + randomInline(r, 2) + eol + "- [ ] " + randomInline(r, 2) + eol
	case 2:
		return "3. " + randomInline(r, 2) + eol + "4. " + randomInline(r, 2) + eol + eol + "   " + randomInline(r, 2) + eol
	case 3:
		return "> " + randomInline(r, 3) + eol + "> > " + randomInline(r, 2) + eol + "lazy " + randomInline(r, 1) + eol
	case 4:
		return "| " + randomInline(r, 1) + " | b \\| c |" + eol + "|:--|--:|" + eol + "| `x\\|y` | " + randomInline(r, 2) + " |" + eol
	default:
		return "```go" + eol + "func main() {" + eol + "\tfmt.Println(\"" + randomInline(r, 2) + "\")" + eol + "}" + eol + "```" + eol
	}
}

func randomBlockTail(r *rand.Rand, eol string, choice int) string {
	switch choice {
	case 6:
		return "~~~mermaid" + eol + "graph TD; A-->B" + eol + "~~~" + eol
	case 7:
		return "$$" + eol + "\\frac{a}{b} " + randomInline(r, 1) + eol + "$$" + eol
	case 8:
		return "<div>" + eol + randomInline(r, 2) + eol + "</div>" + eol
	case 9:
		return "---" + eol
	case 10:
		return "[ref]: https://ref.example \"title\"" + eol
	default:
		return randomInline(r, 4) + "  " + eol + randomInline(r, 3) + "\\" + eol + randomInline(r, 2) + eol
	}
}

func randomMarkdown(r *rand.Rand) string {
	eol := "\n"
	if r.IntN(3) == 0 {
		eol = "\r\n"
	}
	var b strings.Builder
	for range 1 + r.IntN(8) {
		b.WriteString(randomBlock(r, eol))
		if r.IntN(2) == 0 {
			b.WriteString(eol)
		}
	}
	if r.IntN(4) == 0 {
		b.WriteString("```" + eol + randomInline(r, 2))
	}
	return b.String()
}

func committed(t *testing.T, item string) Identity {
	t.Helper()
	id, err := Committed("local", "thread-1", "turn-1", item)
	if err != nil {
		t.Fatal(err)
	}
	return id
}

func markdown(t *testing.T, item, source string) *Document {
	t.Helper()
	d, err := New(Header{Identity: committed(t, item), Format: FormatMarkdown}, source)
	if err != nil {
		t.Fatal(err)
	}
	return d
}

func decodedOracle(source string) string {
	if strings.HasPrefix(source, "\\") {
		return source[1:]
	}
	if source == "&#0;" {
		return "\uFFFD"
	}
	return html.UnescapeString(source)
}

func checkRun(t *testing.T, source string, text string, run Run) {
	t.Helper()
	got := text[run.Text.Start:run.Text.End]
	from := source[run.Source.Start:run.Source.End]
	switch run.Kind {
	case Verbatim:
		checkVerbatimRun(t, got, from)
	case Decoded:
		checkDecodedRun(t, got, from)
	case Break:
		checkBreakRun(t, got, from)
	case Generated:
		checkGeneratedRun(t, got, from)
	case Formula:
		checkFormulaRun(t, got, from)
	default:
		t.Fatalf("run kind %d", run.Kind)
	}
}

func checkVerbatimRun(t *testing.T, got, from string) {
	t.Helper()
	if got != from {
		t.Fatalf("verbatim run %q maps to source %q", got, from)
	}
}

func checkDecodedRun(t *testing.T, got, from string) {
	t.Helper()
	if want := decodedOracle(from); got != want {
		t.Fatalf("decoded run %q maps to source %q, which decodes to %q", got, from, want)
	}
}

func checkBreakRun(t *testing.T, got, from string) {
	t.Helper()
	if !isBreakText(got) || !isBreakSource(from) {
		t.Fatalf("break run %q maps to source %q", got, from)
	}
}

func isBreakText(got string) bool {
	return got == " " || got == "\n" || got == "\t"
}

func isBreakSource(from string) bool {
	return from != "" && strings.Trim(from, " \t\r\n|:-\\") == ""
}

func checkGeneratedRun(t *testing.T, got, from string) {
	t.Helper()
	if !isGeneratedText(got) || !isGeneratedSource(from) {
		t.Fatalf("generated run %q maps to source %q", got, from)
	}
}

func isGeneratedText(got string) bool {
	return strings.Trim(got, " ") == "" || got == "image: " || got == "\t"
}

func isGeneratedSource(from string) bool {
	return from == "" || from == "![" || from == "\t"
}

func checkFormulaRun(t *testing.T, got, from string) {
	t.Helper()
	if want := formulaOracle(from); got != want {
		t.Fatalf("formula run %q maps to source %q, whose body is %q", got, from, want)
	}
}

func formulaOracle(source string) string {
	for _, pair := range [][2]string{{"$$", "$$"}, {"$", "$"}, {`\(`, `\)`}, {`\[`, `\]`}} {
		if strings.HasPrefix(source, pair[0]) && strings.HasSuffix(source, pair[1]) && len(source) >= len(pair[0])+len(pair[1]) {
			return strings.Trim(source[len(pair[0]):len(source)-len(pair[1])], " \t\r\n")
		}
	}
	return "no math delimiters"
}

func checkProjection(t *testing.T, d *Document) {
	t.Helper()
	source := d.Source()
	previousEnd := 0
	for i := range d.Len() {
		b := d.Block(i)
		checkBlockSpan(t, i, b, source, previousEnd)
		previousEnd = b.Span.End
		checkBlockRuns(t, i, b, source)
		checkRows(t, b)
	}
}

func checkBlockSpan(t *testing.T, i int, b Block, source string, previousEnd int) {
	t.Helper()
	if b.Span.Start < previousEnd || b.Span.End < b.Span.Start || b.Span.End > len(source) {
		t.Fatalf("block %d span %v overlaps or leaves the source (previous end %d)", i, b.Span, previousEnd)
	}
}

func checkBlockRuns(t *testing.T, i int, b Block, source string) {
	t.Helper()
	text := b.Projection.Text
	textEnd, sourceEnd := 0, b.Span.Start
	for _, run := range b.Projection.Runs {
		checkRunTilesText(t, i, run, textEnd)
		checkRunKeepsSourceOrder(t, i, run, b.Span.End, sourceEnd)
		checkRun(t, source, text, run)
		textEnd, sourceEnd = run.Text.End, run.Source.End
	}
	if textEnd != len(text) {
		t.Fatalf("block %d runs cover %d of %d projected bytes", i, textEnd, len(text))
	}
}

func checkRunTilesText(t *testing.T, i int, run Run, textEnd int) {
	t.Helper()
	if run.Text.Start != textEnd || run.Text.End <= run.Text.Start {
		t.Fatalf("block %d runs do not tile the projection at %d: %+v", i, textEnd, run)
	}
}

func checkRunKeepsSourceOrder(t *testing.T, i int, run Run, spanEnd, sourceEnd int) {
	t.Helper()
	if run.Source.Start < sourceEnd || run.Source.End < run.Source.Start || run.Source.End > spanEnd {
		t.Fatalf("block %d run %+v leaves source order or span end %d", i, run, spanEnd)
	}
}

func checkRows(t *testing.T, b Block) {
	t.Helper()
	at := 0
	for _, row := range b.Rows {
		if row.Start != at || row.End < row.Start {
			t.Fatalf("rows %v do not tile projection of %d bytes", b.Rows, len(b.Projection.Text))
		}
		at = row.End
	}
	if at != len(b.Projection.Text) {
		t.Fatalf("rows %v end at %d, projection has %d bytes", b.Rows, at, len(b.Projection.Text))
	}
}

func checkPositions(t *testing.T, d *Document) {
	t.Helper()
	for i := range d.Len() {
		b := d.Block(i)
		if len(b.Projection.Runs) == 0 {
			continue
		}
		for _, boundary := range inputtext.Boundaries(b.Projection.Text) {
			if _, inside := b.Projection.insideAtomic(boundary); inside {
				continue
			}
			for _, affinity := range []Affinity{Before, After} {
				checkRoundTrip(t, d, b.Projection, boundary, affinity)
			}
		}
	}
}

func checkRoundTrip(t *testing.T, d *Document, p Projection, boundary int, affinity Affinity) {
	t.Helper()
	source := p.sourceOffset(boundary, affinity)
	if source < len(d.Source()) && !utf8.RuneStart(d.Source()[source]) {
		t.Fatalf("boundary %d maps inside a scalar at %d", boundary, source)
	}
	back := p.textOffset(source, affinity)
	if again := p.sourceOffset(back, affinity); again != source {
		t.Fatalf("boundary %d maps to source %d, back to %d, then to source %d (%v)", boundary, source, back, again, affinity)
	}
	for _, run := range p.Runs {
		if boundary != back && run.Text.Start < max(boundary, back) && run.Text.End > min(boundary, back) && run.Source.Len() > 0 {
			t.Fatalf("boundary %d maps to source %d and back to %d across source-backed run %+v (%v)", boundary, source, back, run, affinity)
		}
	}
}

func TestEveryProjectedRangeMapsToItsSourceBytes(t *testing.T) {
	for seed := range uint64(400) {
		r := rand.New(rand.NewPCG(seed, 0x5eed))
		source := randomMarkdown(r)
		d := markdown(t, "item", source)
		checkProjection(t, d)
		checkPositions(t, d)
	}
}

func TestProjectionKeepsTypedMarkdownData(t *testing.T) {
	source := "## Title &amp; *x*\n\n7. [ ] open\n8. [x] done\n\n| a | b |\n|:--|--:|\n| 1 | 2 |\n\n```Go\ncode\n```\n\n```d2\na -> b\n```\n\n$$\nx^2\n$$\n\n[r]: https://r.example\n\nsee [r] and ![pic](https://p.example/a.png)\n"
	d := markdown(t, "typed", source)
	checkTypedKinds(t, d)
	checkTypedHeadingAndLists(t, d)
	checkTypedTableAndCode(t, d)
	checkTypedFigureMathAndReference(t, d)
}

func checkTypedKinds(t *testing.T, d *Document) {
	t.Helper()
	kinds := []Kind{KindHeading, KindParagraph, KindParagraph, KindTable, KindCode, KindFigure, KindMath, KindParagraph}
	if d.Len() != len(kinds) {
		t.Fatalf("got %d blocks, want %d", d.Len(), len(kinds))
	}
	for i, kind := range kinds {
		if got := d.Block(i).Kind; got != kind {
			t.Fatalf("block %d kind %d, want %d", i, got, kind)
		}
	}
}

func checkTypedHeadingAndLists(t *testing.T, d *Document) {
	t.Helper()
	if b := d.Block(0); b.Level != 2 || b.Projection.Text != "Title & x" {
		t.Fatalf("heading level %d text %q", b.Level, b.Projection.Text)
	}
	open, done := d.Block(1), d.Block(2)
	if open.Task != TaskOpen || done.Task != TaskDone || open.Containers[0].Start != 7 || done.Containers[0].Number != 8 || !open.Containers[0].Ordered {
		t.Fatalf("list items %+v %+v", open, done)
	}
}

func checkTypedTableAndCode(t *testing.T, d *Document) {
	t.Helper()
	if table := d.Block(3); table.Projection.Text != "a\tb\n1\t2" || len(table.Rows) != 2 ||
		table.Alignments[0] != AlignLeft || table.Alignments[1] != AlignRight {
		t.Fatalf("table %q rows %v alignments %v", table.Projection.Text, table.Rows, table.Alignments)
	}
	if code := d.Block(4); code.Language != "Go" || code.Projection.Text != "code\n" {
		t.Fatalf("code %q %q", code.Language, code.Projection.Text)
	}
}

func checkTypedFigureMathAndReference(t *testing.T, d *Document) {
	t.Helper()
	if figure := d.Block(5); figure.Projection.Text != "d2\na -> b\n" || figure.Projection.Text[figure.Body.Start:figure.Body.End] != "a -> b\n" {
		t.Fatalf("figure %q body %v", figure.Projection.Text, figure.Body)
	}
	if math := d.Block(6); math.Projection.Text != "x^2" {
		t.Fatalf("math %q", math.Projection.Text)
	}
	last := d.Block(7)
	if last.Projection.Text != "see r and image: pic" || len(last.Links) != 1 || last.Links[0].Destination != "https://r.example" {
		t.Fatalf("reference paragraph %q links %+v", last.Projection.Text, last.Links)
	}
}

func TestMathDelimitersFollowTheDollarRules(t *testing.T) {
	cases := []struct {
		source string
		math   []string
	}{
		{"cost $5 and $6 total", nil},
		{"value $x$ here", []string{"x"}},
		{"open $ x$ no", nil},
		{"close $x $ no", nil},
		{"digit $x$5 no", nil},
		{"escaped \\$x$ no", nil},
		{"code `$x$` no", nil},
		{"inline \\(a+b\\) and \\[c\\]", []string{"a+b", "c"}},
		{"double $$y$$ inline", []string{"y"}},
	}
	for _, c := range cases {
		d := markdown(t, "math", c.source)
		var got []string
		for _, run := range d.Block(0).Projection.Runs {
			if run.Style&InlineMath != 0 {
				got = append(got, d.Block(0).Projection.Text[run.Text.Start:run.Text.End])
			}
		}
		if strings.Join(got, "|") != strings.Join(c.math, "|") {
			t.Errorf("%q: math %q, want %q", c.source, got, c.math)
		}
	}
	fence := markdown(t, "fence", "```\n$x$\n```\n")
	if fence.Block(0).Kind != KindCode || fence.Block(0).Projection.Text != "$x$\n" {
		t.Fatalf("fence reinterpreted: %+v", fence.Block(0))
	}
}

func TestLinksOpenOnlyAllowedSchemes(t *testing.T) {
	cases := []struct {
		destination string
		opens       bool
	}{
		{"https://example.com/a", true},
		{"HTTP://example.com", true},
		{"mailto:a@example.com", true},
		{"javascript:alert(1)", false},
		{"file:///etc/passwd", false},
		{"/relative", false},
		{"https://", false},
		{"mailto:", false},
		{"http://%zz", false},
	}
	for _, c := range cases {
		_, err := Link{Destination: c.destination}.Target()
		if (err == nil) != c.opens || (err != nil && !errors.Is(err, ErrLink)) {
			t.Errorf("%q: err %v, opens %v", c.destination, err, c.opens)
		}
	}
}

func TestIdentityKeepsTurnsAndEndpointsApart(t *testing.T) {
	a, b, _ := distinctIdentities(t)
	checkMissingTurnRejected(t)
	draft := checkDraftIdentity(t, a)
	checkDraftAssign(t, draft, a, b)
}

func distinctIdentities(t *testing.T) (Identity, Identity, Identity) {
	t.Helper()
	a, _ := Committed("local", "thread", "turn-1", "item")
	b, _ := Committed("local", "thread", "turn-2", "item")
	c, _ := Committed("remote", "thread", "turn-1", "item")
	if a == b || a == c {
		t.Fatalf("identities collide: %+v %+v %+v", a, b, c)
	}
	return a, b, c
}

func checkMissingTurnRejected(t *testing.T) {
	t.Helper()
	if _, err := Committed("local", "thread", "", "item"); !errors.Is(err, ErrIdentity) {
		t.Fatalf("missing turn accepted: %v", err)
	}
}

func checkDraftIdentity(t *testing.T, a Identity) Identity {
	t.Helper()
	draft, err := LocalDraft("local", "thread", "draft-1")
	if err != nil || !draft.IsDraft() || draft == a {
		t.Fatalf("draft %+v %v", draft, err)
	}
	return draft
}

func checkDraftAssign(t *testing.T, draft, a, b Identity) {
	t.Helper()
	d, _ := New(Header{Identity: draft, Format: FormatMarkdown}, "hello *there*")
	committedDoc, change, err := d.Assign(a)
	if err != nil {
		t.Fatal(err)
	}
	if committedDoc.Identity() != a || committedDoc.Revision() != d.Revision()+1 || len(change.Keys) != 1 ||
		change.Keys[0].From.Document != draft || change.Keys[0].To.Document != a {
		t.Fatalf("assign %+v %+v", committedDoc.Identity(), change)
	}
	if _, _, err := committedDoc.Assign(b); !errors.Is(err, ErrIdentity) {
		t.Fatalf("committed document reassigned: %v", err)
	}
}

func TestSourceStaysImmutableAcrossRevisions(t *testing.T) {
	d := markdown(t, "imm", "first")
	open, _ := New(Header{Identity: committed(t, "imm2"), Format: FormatMarkdown, Open: true}, "first")
	next, _, err := open.Append(" second")
	if err != nil {
		t.Fatal(err)
	}
	branch, _, err := open.Append(" other")
	if err != nil {
		t.Fatal(err)
	}
	if open.Source() != "first" || next.Source() != "first second" || branch.Source() != "first other" || d.Source() != "first" {
		t.Fatalf("sources changed: %q %q %q", open.Source(), next.Source(), branch.Source())
	}
	if _, _, err := d.Append("x"); !errors.Is(err, ErrClosed) {
		t.Fatalf("closed document accepted output: %v", err)
	}
	if _, err := New(Header{Identity: committed(t, "bad"), Format: FormatMarkdown}, "ok\xffno"); !errors.Is(err, ErrEncoding) {
		t.Fatalf("invalid UTF-8 accepted: %v", err)
	}
}

func TestClosedTailFigureGetsFinalKey(t *testing.T) {
	d, err := New(Header{Identity: committed(t, "fence-close"), Format: FormatMarkdown, Open: true}, "```mermaid\ngraph TD\nA-->B\n```\n")
	if err != nil {
		t.Fatal(err)
	}
	if d.Len() != 1 || d.Block(0).Kind != KindFigure {
		t.Fatalf("tail blocks = %d, want one figure", d.Len())
	}
	if d.Block(0).Key.Provisional {
		t.Fatal("closed tail figure kept a provisional key, so the transcript would show source instead of the figure")
	}
}

func TestOpenTailFigureStaysProvisional(t *testing.T) {
	d, err := New(Header{Identity: committed(t, "fence-open"), Format: FormatMarkdown, Open: true}, "```mermaid\ngraph TD\nA-->B\n")
	if err != nil {
		t.Fatal(err)
	}
	if d.Len() != 1 || d.Block(0).Kind != KindFigure {
		t.Fatalf("tail blocks = %d, want one figure", d.Len())
	}
	if !d.Block(0).Key.Provisional {
		t.Fatal("open tail figure got a final key, so later fence lines would orphan the dispatched figure")
	}
}

func TestFigureStaysFinalWhenMessageGrows(t *testing.T) {
	d, err := New(Header{Identity: committed(t, "fence-grow"), Format: FormatMarkdown, Open: true}, "```mermaid\ngraph TD\nA-->B\n```\n")
	if err != nil {
		t.Fatal(err)
	}
	next, _, err := d.Append("\nnext paragraph\n")
	if err != nil {
		t.Fatal(err)
	}
	if next.Block(0).Key.Provisional {
		t.Fatal("closed figure turned provisional when later text arrived")
	}
	if !next.Block(1).Key.Provisional {
		t.Fatal("growing tail paragraph got a final key")
	}
}
