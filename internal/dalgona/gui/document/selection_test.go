package document

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

type pages struct {
	sources map[Identity]string
	size    int
	reads   int
	fail    error
	overrun bool
}

func (p *pages) Page(_ context.Context, ref Ref, offset, limit int) (string, error) {
	p.reads++
	if p.fail != nil {
		return "", p.fail
	}
	source := p.sources[ref.Header.Identity]
	end := min(len(source), offset+min(limit, p.size))
	if p.overrun {
		return source[offset:min(len(source), offset+limit)] + "!", nil
	}
	return source[offset:end], nil
}

func positionAt(t *testing.T, d *Document, needle string, delta int, affinity Affinity) Position {
	t.Helper()
	at := strings.Index(d.Source(), needle)
	if at < 0 {
		t.Fatalf("%q is not in %q", needle, d.Source())
	}
	p, err := d.Position(at+delta, affinity)
	if err != nil {
		t.Fatal(err)
	}
	return p
}

func TestSelectionAcrossBlocksCopiesSourceText(t *testing.T) {
	source := "Alpha **bold** 한국어 👩\u200d👩\u200d👧\n\n```go\r\nfmt.Println(\"x\")\r\n```\r\n\n| h1 | h2 |\n|--|--|\n| a\\|b | `c` |\n"
	d := markdown(t, "copy", source)
	from := positionAt(t, d, "bold", 0, After)
	to := positionAt(t, d, "Println", len("Println"), Before)

	text, err := d.Text(from, to)
	if err != nil {
		t.Fatal(err)
	}
	if want := "bold 한국어 👩\u200d👩\u200d👧\n\nfmt.Println"; text != want {
		t.Fatalf("copied %q, want %q", text, want)
	}
	markdownText, err := d.Markdown(from, to)
	if err != nil {
		t.Fatal(err)
	}
	if want := source[strings.Index(source, "bold") : strings.Index(source, "Println")+len("Println")]; markdownText != want {
		t.Fatalf("copied Markdown %q, want the exact source slice %q", markdownText, want)
	}
	reversed, _ := d.Text(to, from)
	if reversed != text {
		t.Fatalf("reversed selection copies %q", reversed)
	}

	codeStart, _ := d.Position(strings.Index(source, "fmt"), After)
	tableEnd, _ := d.End()
	whole, _ := d.Text(codeStart, tableEnd)
	if want := "fmt.Println(\"x\")\r\n\nh1\th2\na|b\tc"; whole != want {
		t.Fatalf("code and table copy %q, want %q", whole, want)
	}
}

func TestScopeCopyPagesUnloadedDocuments(t *testing.T) {
	fixture := pagedScope(t)
	text, err := fixture.scope.Copy(context.Background(), fixture.sel, fixture.pager)
	if err != nil {
		t.Fatal(err)
	}
	checkPagedCopy(t, fixture.scope, text, fixture.pager, fixture.middleSource)
	checkCopyRejectsOverrun(t, fixture.scope, fixture.sel, fixture.pager)
	checkCopyRejectsFailure(t, fixture.scope, fixture.sel, fixture.pager)
}

type pagedFixture struct {
	scope        *Scope
	sel          Selection
	pager        *pages
	middleSource string
}

func pagedScope(t *testing.T) pagedFixture {
	t.Helper()
	scope := NewScope("local", "thread-1")
	first := markdown(t, "a", "first *message* text")
	middleSource := "middle **loaded** later\n\n- item\n"
	middleID := committed(t, "b")
	last := markdown(t, "c", "last line")
	if _, err := scope.Put(first); err != nil {
		t.Fatal(err)
	}
	if err := scope.Prepend(); err != nil {
		t.Fatal(err)
	}
	scope.entries = append(scope.entries, entry{ref: Ref{Header: Header{Identity: middleID, Format: FormatMarkdown}, Revision: 3, Length: len(middleSource)}})
	scope.reindex()
	if _, err := scope.Put(last); err != nil {
		t.Fatal(err)
	}
	pager := &pages{sources: map[Identity]string{middleID: middleSource}, size: 5}
	sel := Selection{Anchor: positionAt(t, first, "message", 0, After), Focus: positionAt(t, last, "line", 0, Before)}
	return pagedFixture{scope: scope, sel: sel, pager: pager, middleSource: middleSource}
}

func checkPagedCopy(t *testing.T, scope *Scope, text string, pager *pages, middleSource string) {
	t.Helper()
	if want := "message text\n\nmiddle loaded later\n\nitem\n\nlast "; text != want {
		t.Fatalf("copied %q, want %q", text, want)
	}
	if pager.reads < len(middleSource)/5 {
		t.Fatalf("unloaded document read in %d pages", pager.reads)
	}
	if _, ok := scope.Loaded(1); ok {
		t.Fatal("copy left the paged document resident")
	}
}

func checkCopyRejectsOverrun(t *testing.T, scope *Scope, sel Selection, pager *pages) {
	t.Helper()
	if text, err := scope.Copy(context.Background(), sel, &pages{sources: pager.sources, size: 1, overrun: true}); !errors.Is(err, ErrPage) || text != "" {
		t.Fatalf("a page longer than its request was accepted: %q, %v", text, err)
	}
}

func checkCopyRejectsFailure(t *testing.T, scope *Scope, sel Selection, pager *pages) {
	t.Helper()
	pager.fail = errors.New("connection lost")
	if text, err := scope.Copy(context.Background(), sel, pager); !errors.Is(err, ErrPage) || text != "" {
		t.Fatalf("failed page copy returned %q, %v", text, err)
	}
}

func TestCopyRefusesSelectionsAboveTheLimit(t *testing.T) {
	scope := NewScope("local", "thread-1")
	small := markdown(t, "a", "small")
	if _, err := scope.Put(small); err != nil {
		t.Fatal(err)
	}
	huge := committed(t, "huge")
	scope.entries = append(scope.entries, entry{ref: Ref{Header: Header{Identity: huge, Format: FormatCode}, Revision: 1, Length: MaxCopyBytes + 1}})
	scope.reindex()
	pager := &pages{}
	sel := Selection{Anchor: scope.Edge(0, false), Focus: scope.Edge(1, true)}
	if text, err := scope.Copy(context.Background(), sel, pager); !errors.Is(err, ErrCopyLimit) || text != "" || pager.reads != 0 {
		t.Fatalf("oversized copy returned %d bytes, %v, after %d reads", len(text), err, pager.reads)
	}
}

func TestMovementCrossesWholeGraphemesAndWords(t *testing.T) {
	d := markdown(t, "move", "a👩\u200d👩\u200d👧b 日本語 hello-world\n\nnext")
	p, _ := d.Start()
	next, _, _ := d.Next(p, StepGrapheme)
	next, _, _ = d.Next(next, StepGrapheme)
	if want := strings.Index(d.Source(), "b "); next.Offset != want {
		t.Fatalf("two grapheme steps reached %d, want %d past the family emoji", next.Offset, want)
	}
	word, err := d.Word(positionAt(t, d, "本", 0, After))
	if err != nil {
		t.Fatal(err)
	}
	if text, _ := d.Text(word.Anchor, word.Focus); text != "本" {
		t.Fatalf("ideograph word %q", text)
	}
	word, _ = d.Word(positionAt(t, d, "llo", 0, After))
	if text, _ := d.Text(word.Anchor, word.Focus); text != "hello" {
		t.Fatalf("word %q", text)
	}
	end, _ := d.Position(strings.Index(d.Source(), "\n"), Before)
	crossed, moved, _ := d.Next(end, StepWord)
	if !moved || crossed.Offset != strings.Index(d.Source(), "next") {
		t.Fatalf("word step from the block end reached %d", crossed.Offset)
	}
	back, _, _ := d.Previous(crossed, StepGrapheme)
	if back.Offset != end.Offset {
		t.Fatalf("previous from the block start reached %d, want %d", back.Offset, end.Offset)
	}
	last, _ := d.End()
	if _, moved, _ := d.Next(last, StepGrapheme); moved {
		t.Fatal("movement past the document end")
	}
}

func TestFindSearchesEveryDocumentInScope(t *testing.T) {
	scope := NewScope("local", "thread-1")
	loaded := markdown(t, "a", "needle *needle*")
	id := committed(t, "b")
	unloaded := "no match\n\n`needle` here"
	if _, err := scope.Put(loaded); err != nil {
		t.Fatal(err)
	}
	scope.entries = append(scope.entries, entry{ref: Ref{Header: Header{Identity: id, Format: FormatMarkdown}, Revision: 1, Length: len(unloaded)}})
	scope.reindex()
	matches, err := scope.Find(context.Background(), "needle", &pages{sources: map[Identity]string{id: unloaded}, size: 4})
	if err != nil {
		t.Fatal(err)
	}
	if matches.Documents != 2 || matches.Count != 3 || len(matches.Results) != 3 || matches.Results[2].Anchor.Document != id {
		t.Fatalf("matches %+v", matches)
	}
}

func TestUnitConversionsSeparateBytesScalarsAndUTF16(t *testing.T) {
	d := markdown(t, "units", "**é😀** x")
	p := positionAt(t, d, " x", 0, After)
	_, bytes, _ := d.TextOffset(p, textinput.Bytes)
	key, units, err := d.TextOffset(p, textinput.UTF16Units)
	if err != nil || bytes != len("é😀") || units != 3 || key != d.Block(0).Key {
		t.Fatalf("offsets bytes %d utf16 %d key %+v (%v)", bytes, units, key, err)
	}
}

const formulaSource = "area $\\pi r^2$ and \\(e^x\\) here"

func TestInlineFormulaSelectsAsOneUnit(t *testing.T) {
	d := markdown(t, "formula", formulaSource)
	opening, closing := strings.Index(formulaSource, "$"), strings.LastIndex(formulaSource, "$")+1
	if p := positionAt(t, d, "r^2", 0, After); p.Offset != opening {
		t.Fatalf("a position inside the formula resolved to %d, want the formula start %d", p.Offset, opening)
	}
	if p := positionAt(t, d, "r^2", 0, Before); p.Offset != closing {
		t.Fatalf("a position inside the formula resolved to %d before, want the formula end %d", p.Offset, closing)
	}
	before, _ := d.Position(opening, After)
	next, moved, err := d.Next(before, StepGrapheme)
	if err != nil || !moved || next.Offset != closing {
		t.Fatalf("one step over the formula reached %d (%v), want %d", next.Offset, err, closing)
	}
}

func TestCopyAcrossInlineFormulasKeepsTheirExactSource(t *testing.T) {
	d := markdown(t, "formula", formulaSource)
	start, _ := d.Start()
	end, _ := d.End()
	if text, err := d.Text(start, end); err != nil || text != "area \\pi r^2 and e^x here" {
		t.Fatalf("copied %q (%v), want each formula's exact TeX", text, err)
	}
	from := positionAt(t, d, "area", len("ar"), After)
	to := positionAt(t, d, "here", len("he"), Before)
	want := formulaSource[strings.Index(formulaSource, "area")+len("ar") : strings.Index(formulaSource, "here")+len("he")]
	if markdownText, err := d.Markdown(from, to); err != nil || markdownText != want {
		t.Fatalf("Markdown copy %q (%v), want the exact source %q", markdownText, err, want)
	}
}
