//go:build gui

package platform

import (
	"context"
	"errors"
	"image"
	"image/color"
	"testing"
	"time"
	"unicode/utf8"

	"gioui.org/font/gofont"
	"gioui.org/io/input"
	"gioui.org/io/key"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/paint"
	"gioui.org/text"
	"gioui.org/unit"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

type editorRig struct {
	t      *testing.T
	router input.Router
	editor *Editor
	shaper *text.Shaper
	ime    gioIME
}

func newEditorRig(t *testing.T, policy textinput.Policy, draft textinput.Draft) *editorRig {
	t.Helper()
	ed, err := NewEditor(policy, draft)
	if err != nil {
		t.Fatal(err)
	}
	rig := &editorRig{t: t, editor: ed, shaper: text.NewShaper(text.NoSystemFonts(), text.WithCollection(gofont.Collection()))}
	rig.ime = gioIME{router: &rig.router, compose: key.Range{Start: -1, End: -1}}
	rig.frame()
	rig.router.Source().Execute(key.FocusCmd{Tag: ed.Tag()})
	rig.frame()
	return rig
}

func colorMaterial(ops *op.Ops, c color.NRGBA) op.CallOp {
	m := op.Record(ops)
	paint.ColorOp{Color: c}.Add(ops)
	return m.Stop()
}

func (r *editorRig) frame() []EditorEvent {
	ops := new(op.Ops)
	gtx := layout.Context{
		Ops:         ops,
		Source:      r.router.Source(),
		Constraints: layout.Exact(image.Pt(640, 160)),
		Metric:      unit.Metric{PxPerDp: 1, PxPerSp: 1},
	}
	var events []EditorEvent
	for {
		ev, ok := r.editor.Update(gtx)
		if !ok {
			break
		}
		events = append(events, ev)
	}
	r.editor.Layout(gtx, EditorStyle{
		Shaper:    r.shaper,
		Size:      16,
		Text:      colorMaterial(ops, color.NRGBA{A: 0xff}),
		Selection: colorMaterial(ops, color.NRGBA{B: 0xff, A: 0x40}),
	})
	r.router.Frame(ops)
	r.ime.sync()
	return events
}

func (r *editorRig) press(name key.Name, mods key.Modifiers) {
	r.router.Queue(key.Event{Name: name, Modifiers: mods, State: key.Press})
}

func (r *editorRig) wantDraft(want textinput.Draft) {
	r.t.Helper()
	if got := r.editor.Draft(); got != want {
		r.t.Fatalf("draft %+v, want %+v", got, want)
	}
}

func (r *editorRig) wantShown(want string) {
	r.t.Helper()
	if got := r.editor.widget.Text(); got != want {
		r.t.Fatalf("the widget shows %q, want %q", got, want)
	}
}

func (r *editorRig) wantComposing(want bool) {
	r.t.Helper()
	if got := r.editor.Composing(); got != want {
		r.t.Fatalf("composing %v, want %v", got, want)
	}
}

func editorEventKinds(events []EditorEvent) []EditorEventKind {
	out := make([]EditorEventKind, 0, len(events))
	for _, ev := range events {
		out = append(out, ev.Kind)
	}
	return out
}

func hasEditorEvent(events []EditorEvent, kind EditorEventKind) bool {
	for _, ev := range events {
		if ev.Kind == kind {
			return true
		}
	}
	return false
}

type gioIME struct {
	router  *input.Router
	state   input.EditorState
	compose key.Range
}

func (p *gioIME) sync() {
	p.state = p.router.EditorState()
}

func gioNorm(r key.Range) key.Range {
	if r.Start > r.End {
		r.Start, r.End = r.End, r.Start
	}
	return r
}

func gioAdjust(pos int, r key.Range, newEnd int) int {
	switch {
	case newEnd < pos && pos <= r.End:
		return newEnd
	case r.End < pos:
		return pos + newEnd - r.End
	}
	return pos
}

func (p *gioIME) imeRange() key.Range {
	rng := p.compose
	if rng.Start == -1 {
		rng = p.state.Selection.Range
	}
	return gioNorm(rng)
}

func (p *gioIME) replace(r key.Range, s string) {
	n := gioNorm(r)
	runes := []rune(s)
	newEnd := n.Start + len(runes)
	p.state.Selection.Start = gioAdjust(p.state.Selection.Start, n, newEnd)
	p.state.Selection.End = gioAdjust(p.state.Selection.End, n, newEnd)
	if p.compose.Start != -1 {
		p.compose = key.Range{Start: gioAdjust(p.compose.Start, n, newEnd), End: gioAdjust(p.compose.End, n, newEnd)}
	}
	p.replaceSnippet(n, runes)
	p.router.Queue(key.EditEvent{Range: r, Text: s}, key.SnippetEvent(p.state.Snippet.Range))
}

func (p *gioIME) replaceSnippet(r key.Range, runes []rune) {
	s := p.state.Snippet
	if r.End < s.Start || r.Start > s.End {
		s = key.Snippet{Range: key.Range{Start: r.Start, End: r.Start}}
	}
	old := []rune(s.Text)
	var next []rune
	if end := r.Start - s.Start; end > 0 {
		next = append(next, old[:end]...)
	}
	next = append(next, runes...)
	if r.End < s.End {
		next = append(next, old[r.End-s.Start:]...)
	}
	s.Start = min(s.Start, r.Start)
	s.End = s.Start + len(next)
	s.Text = string(next)
	p.state.Snippet = s
}

func (p *gioIME) setComposingRegion(r key.Range) {
	p.compose = r
	p.router.Queue(key.CompositionEvent(r))
}

func (p *gioIME) setSelection(r key.Range) {
	p.state.Selection.Range = r
	p.router.Queue(key.SelectionEvent(r))
}

func (p *gioIME) insert(s string) {
	sel := p.state.Selection.Range
	p.replace(sel, s)
	pos := min(sel.Start, sel.End) + utf8.RuneCountInString(s)
	p.setSelection(key.Range{Start: pos, End: pos})
}

func (p *gioIME) windowsResult(result string) {
	rng := p.imeRange()
	p.replace(rng, result)
	end := rng.Start + utf8.RuneCountInString(result)
	p.setComposingRegion(key.Range{Start: -1, End: -1})
	p.setSelection(key.Range{Start: end, End: end})
}

func (p *gioIME) windowsComposition(composition string) {
	rng := p.imeRange()
	end := rng.Start + utf8.RuneCountInString(composition)
	p.replace(rng, composition)
	p.setComposingRegion(key.Range{Start: rng.Start, End: end})
	p.setSelection(key.Range{Start: end, End: end})
}

func (p *gioIME) windowsEndComposition() {
	p.setComposingRegion(key.Range{Start: -1, End: -1})
}

func (p *gioIME) macSetMarkedText(marked string) {
	rng := p.compose
	if rng.Start == -1 {
		rng = p.state.Selection.Range
	}
	p.replace(rng, marked)
	comp := key.Range{Start: rng.Start, End: rng.Start + utf8.RuneCountInString(marked)}
	p.setComposingRegion(comp)
	p.setSelection(key.Range{Start: comp.End, End: comp.End})
}

func (p *gioIME) macInsertText(inserted string) {
	rng := p.compose
	if rng.Start == -1 {
		rng = p.state.Selection.Range
	}
	p.replace(rng, inserted)
	p.setComposingRegion(key.Range{Start: -1, End: -1})
	pos := min(rng.Start, rng.End) + utf8.RuneCountInString(inserted)
	p.setSelection(key.Range{Start: pos, End: pos})
}

func (p *gioIME) macUnmarkText() {
	p.setComposingRegion(key.Range{Start: -1, End: -1})
}

type textInputDone struct {
	commit, preedit       string
	commitSet, preeditSet bool
}

func (p *gioIME) waylandDone(u textInputDone) {
	if p.compose.Start != -1 {
		compose := gioNorm(p.compose)
		p.replace(compose, "")
		p.setComposingRegion(key.Range{Start: -1, End: -1})
		p.setSelection(key.Range{Start: compose.Start, End: compose.Start})
	}
	if u.commitSet {
		rng := p.imeRange()
		p.replace(rng, u.commit)
		pos := rng.Start + utf8.RuneCountInString(u.commit)
		p.setSelection(key.Range{Start: pos, End: pos})
	}
	if u.preeditSet {
		rng := p.imeRange()
		p.replace(rng, u.preedit)
		compose := key.Range{Start: rng.Start, End: rng.Start + utf8.RuneCountInString(u.preedit)}
		p.setComposingRegion(compose)
		p.setSelection(key.Range{Start: compose.End, End: compose.End})
	}
}

func TestEditorKoreanCompositionCommitsEachSyllableOnce(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{})
	for _, preedit := range []string{"ㅎ", "하", "한"} {
		rig.ime.windowsComposition(preedit)
		if events := rig.frame(); len(events) != 0 {
			t.Fatalf("preedit %q produced editor events %v", preedit, editorEventKinds(events))
		}
		rig.wantComposing(true)
		rig.wantDraft(textinput.Draft{})
		rig.wantShown(preedit)
	}
	rig.ime.windowsResult("한")
	rig.ime.windowsComposition("ㄱ")
	rig.frame()
	rig.wantComposing(true)
	rig.wantDraft(textinput.Draft{Text: "한", Selection: caretAt(3)})
	rig.wantShown("한ㄱ")
	for _, preedit := range []string{"그", "글"} {
		rig.ime.windowsComposition(preedit)
		rig.frame()
	}
	rig.ime.windowsResult("글")
	rig.ime.windowsEndComposition()
	rig.frame()
	rig.wantComposing(false)
	rig.wantDraft(textinput.Draft{Text: "한글", Selection: caretAt(6)})
	rig.wantShown("한글")
	for _, want := range []textinput.Draft{{Text: "한", Selection: caretAt(3)}, {}, {}} {
		rig.press("Z", key.ModShortcut)
		rig.frame()
		rig.wantDraft(want)
		rig.wantShown(want.Text)
	}
}

func TestEditorKeepsTheSelectionBehindTextInputV3Preedit(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{Text: "abc", Selection: textinput.Range{Start: 0, End: 3}})
	for _, preedit := range []string{"ㅎ", "하"} {
		rig.ime.waylandDone(textInputDone{preedit: preedit, preeditSet: true})
		rig.frame()
		rig.wantComposing(true)
		rig.wantDraft(textinput.Draft{Text: "abc", Selection: textinput.Range{Start: 0, End: 3}})
		rig.wantShown(preedit)
	}
	rig.ime.waylandDone(textInputDone{commit: "한", commitSet: true, preedit: "ㄱ", preeditSet: true})
	rig.frame()
	rig.wantComposing(true)
	rig.wantDraft(textinput.Draft{Text: "한", Selection: caretAt(3)})
	rig.wantShown("한ㄱ")
	rig.ime.waylandDone(textInputDone{})
	rig.frame()
	rig.wantComposing(false)
	rig.wantDraft(textinput.Draft{Text: "한", Selection: caretAt(3)})
	rig.wantShown("한")
}

func TestEditorJapaneseCancelRestoresTextAndSelection(t *testing.T) {
	before := textinput.Draft{Text: "東京は晴れ", Selection: textinput.Range{Start: 9, End: 15}}
	cancels := map[string]func(rig *editorRig){
		"windows imm32": func(rig *editorRig) {
			rig.editor.unmarkCommits = false
			for _, preedit := range []string{"か", "かん", "かんじ", "漢字"} {
				rig.ime.windowsComposition(preedit)
				rig.frame()
			}
			rig.ime.windowsEndComposition()
		},
		"macos": func(rig *editorRig) {
			rig.editor.unmarkCommits = true
			for _, marked := range []string{"かんじ", "漢字", ""} {
				rig.ime.macSetMarkedText(marked)
				rig.frame()
			}
			rig.ime.macUnmarkText()
		},
	}
	for name, cancel := range cancels {
		t.Run(name, func(t *testing.T) {
			rig := newEditorRig(t, prompt(100), before)
			rig.wantDraft(before)
			revision := rig.editor.Revision()
			cancel(rig)
			rig.frame()
			rig.wantComposing(false)
			rig.wantDraft(before)
			rig.wantShown(before.Text)
			if start, end := rig.editor.widget.Selection(); min(start, end) != 3 || max(start, end) != 5 {
				t.Fatalf("widget selection %d..%d, want runes 3..5", start, end)
			}
			if rig.editor.Revision() != revision {
				t.Fatalf("revision moved from %d to %d across a cancelled composition", revision, rig.editor.Revision())
			}
		})
	}
}

func singleLinePolicy(maxScalars int) textinput.Policy {
	return textinput.Policy{Lines: textinput.SingleLine, Limit: textinput.Limit{Unit: textinput.Scalars, Max: maxScalars}}
}

func wantEditorRejection(t *testing.T, events []EditorEvent, want error) {
	t.Helper()
	for _, ev := range events {
		if ev.Kind == EditorRejected && errors.Is(ev.Err, want) {
			return
		}
	}
	t.Fatalf("events %v carry no rejection matching %v", events, want)
}

func TestEditorMacUnmarkTextAcceptsTheMarkedText(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{Text: "東京は晴れ", Selection: textinput.Range{Start: 9, End: 15}})
	rig.editor.unmarkCommits = true
	rig.ime.macSetMarkedText("漢字")
	rig.frame()
	rig.ime.macUnmarkText()
	rig.frame()
	rig.wantComposing(false)
	rig.wantDraft(textinput.Draft{Text: "東京は漢字", Selection: caretAt(15)})
	rig.wantShown("東京は漢字")
}

func TestEditorReturnThatEndsCompositionDoesNotSubmit(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{})
	rig.ime.macSetMarkedText("하")
	rig.frame()
	rig.ime.macInsertText("한")
	rig.press(key.NameReturn, 0)
	if events := rig.frame(); hasEditorEvent(events, EditorSubmit) {
		t.Fatalf("the Return that ended the composition submitted: %v", editorEventKinds(events))
	}
	rig.wantDraft(textinput.Draft{Text: "한", Selection: caretAt(3)})
	rig.press(key.NameReturn, 0)
	events := rig.frame()
	if !hasEditorEvent(events, EditorSubmit) {
		t.Fatalf("a Return in a later frame produced %v, want a submit", editorEventKinds(events))
	}
	rig.wantDraft(textinput.Draft{Text: "한", Selection: caretAt(3)})
}

func TestEditorKeysDuringCompositionStayWithTheInputMethod(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{Text: "a", Selection: caretAt(1)})
	rig.ime.windowsComposition("ㅎ")
	rig.press(key.NameReturn, 0)
	if events := rig.frame(); len(events) != 0 {
		t.Fatalf("Return in the batch that started a composition produced %v", editorEventKinds(events))
	}
	for _, name := range []key.Name{key.NameReturn, key.NameEnter, key.NameEscape, key.NameDeleteBackward, key.NameLeftArrow} {
		rig.press(name, 0)
		if events := rig.frame(); len(events) != 0 {
			t.Fatalf("%s during composition produced %v", name, editorEventKinds(events))
		}
		rig.wantComposing(true)
		rig.wantShown("aㅎ")
		rig.wantDraft(textinput.Draft{Text: "a", Selection: caretAt(1)})
	}
}

func TestEditorRejectsAnOverLimitCommitWhole(t *testing.T) {
	rig := newEditorRig(t, prompt(4), textinput.Draft{Text: "abc", Selection: caretAt(3)})
	rig.ime.windowsComposition("한")
	rig.frame()
	rig.ime.windowsResult("한")
	rig.ime.windowsComposition("ㄱ")
	rig.frame()
	rig.wantDraft(textinput.Draft{Text: "abc한", Selection: caretAt(6)})
	rig.ime.windowsComposition("글")
	rig.frame()
	rig.ime.windowsResult("글")
	rig.ime.windowsEndComposition()
	events := rig.frame()
	wantEditorRejection(t, events, textinput.TextTooLarge)
	rig.wantComposing(false)
	rig.wantDraft(textinput.Draft{Text: "abc한", Selection: caretAt(6)})
	rig.wantShown("abc한")
	if got := rig.editor.TakeRejected(); got != "글" {
		t.Fatalf("recoverable candidate %q, want the whole commit", got)
	}
	if got := rig.editor.TakeRejected(); got != "" {
		t.Fatalf("the candidate was returned twice: %q", got)
	}
}

func TestEditorAcceptsACommitThatJoinsTheAdjacentGrapheme(t *testing.T) {
	rig := newEditorRig(t, prompt(2), textinput.Draft{Text: "e", Selection: caretAt(1)})
	rig.ime.macInsertText("\u0301")
	if events := rig.frame(); hasEditorEvent(events, EditorRejected) {
		t.Fatalf("the joining commit was refused: %+v", events)
	}
	rig.wantDraft(textinput.Draft{Text: "e\u0301", Selection: caretAt(3)})
	rig.ime.macInsertText("x")
	wantEditorRejection(t, rig.frame(), textinput.TextTooLarge)
	rig.wantDraft(textinput.Draft{Text: "e\u0301", Selection: caretAt(3)})
	rig.wantShown("e\u0301")
}

func TestEditorConvertsCommittedLineEndingsOnce(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{})
	rig.ime.macInsertText("a\r\nb\rc\n")
	rig.frame()
	rig.wantDraft(textinput.Draft{Text: "a\nb\nc\n", Selection: caretAt(6)})
	rig.wantShown("a\nb\nc\n")

	single := newEditorRig(t, singleLinePolicy(100), textinput.Draft{Text: "x", Selection: caretAt(1)})
	single.ime.macInsertText("a\r\nb")
	wantEditorRejection(t, single.frame(), textinput.LineBreakNotAllowed)
	single.wantDraft(textinput.Draft{Text: "x", Selection: caretAt(1)})
	single.wantShown("x")
	if got := single.editor.TakeRejected(); got != "a\r\nb" {
		t.Fatalf("recoverable candidate %q, want the source commit", got)
	}
}

func TestEditorUndoRestoresThePreCommitTextAndSelection(t *testing.T) {
	before := textinput.Draft{Text: "東京", Selection: textinput.Range{Start: 0, End: 6}}
	after := textinput.Draft{Text: "한", Selection: caretAt(3)}
	rig := newEditorRig(t, prompt(100), before)
	for _, preedit := range []string{"ㅎ", "하", "한"} {
		rig.ime.windowsComposition(preedit)
		rig.frame()
	}
	rig.ime.windowsResult("한")
	rig.ime.windowsEndComposition()
	rig.frame()
	rig.wantDraft(after)
	rig.press("Z", key.ModShortcut)
	rig.frame()
	rig.wantDraft(before)
	rig.wantShown(before.Text)
	rig.press("Z", key.ModShortcut|key.ModShift)
	rig.frame()
	rig.wantDraft(after)
	rig.wantShown(after.Text)
}

func TestEditorAnswersSnippetRequestsWithTheShownText(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{Text: "hello", Selection: caretAt(5)})
	rig.router.Queue(key.SnippetEvent{Start: 1, End: 4})
	rig.frame()
	want := key.Snippet{Range: key.Range{Start: 1, End: 4}, Text: "ell"}
	if got := rig.router.EditorState().Snippet; got != want {
		t.Fatalf("snippet %+v, want %+v", got, want)
	}
	rig.ime.windowsComposition("ㅎ")
	rig.frame()
	rig.router.Queue(key.SnippetEvent{Start: 0, End: 9})
	rig.frame()
	want = key.Snippet{Range: key.Range{Start: 0, End: 6}, Text: "helloㅎ"}
	if got := rig.router.EditorState().Snippet; got != want {
		t.Fatalf("snippet during composition %+v, want %+v", got, want)
	}
	rig.wantDraft(textinput.Draft{Text: "hello", Selection: caretAt(5)})
}

func TestEditorPublishesCompositionBoundsWhileComposing(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{Text: "ab", Selection: caretAt(2)})
	rig.ime.windowsComposition("ㅎ")
	rig.frame()
	sel := rig.router.EditorState().Selection
	if sel.CompositionBounds.Empty() || sel.Ascent <= 0 {
		t.Fatalf("composition bounds %v, caret %+v; want the preedit area and caret metrics", sel.CompositionBounds, sel.Caret)
	}
	if x := int(sel.Pos.X); x < sel.CompositionBounds.Min.X || x > sel.CompositionBounds.Max.X {
		t.Fatalf("caret x %d lies outside the composition %v", x, sel.CompositionBounds)
	}
	rig.ime.windowsResult("ㅎ")
	rig.ime.windowsEndComposition()
	rig.frame()
	if b := rig.router.EditorState().Selection.CompositionBounds; !b.Empty() {
		t.Fatalf("composition bounds %v after the commit, want none", b)
	}
}

func pasteThroughGio(rig *editorRig, data string) (*trackedStream, []EditorEvent) {
	rig.t.Helper()
	rig.press("V", key.ModShortcut)
	rig.frame()
	if !rig.router.ClipboardRequested() {
		rig.t.Fatal("Ctrl+V did not request the clipboard")
	}
	e, stream := pasteEvent("application/text", data)
	rig.router.Queue(e)
	return stream, rig.frame()
}

func TestEditorPasteRefusesInvalidTextWithoutEditing(t *testing.T) {
	cases := []struct {
		name   string
		policy textinput.Policy
		data   string
		want   error
		kept   string
	}{
		{"nul", prompt(100), "a\x00b", textinput.DisallowedControl, ""},
		{"malformed utf-8", prompt(100), "a\xffb", textinput.InvalidEncoding, ""},
		{"multiline into one line", singleLinePolicy(100), "a\nb", textinput.LineBreakNotAllowed, "a\nb"},
		{"over the field limit", singleLinePolicy(3), "abc", textinput.TextTooLarge, "abc"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			before := textinput.Draft{Text: "x", Selection: caretAt(1)}
			rig := newEditorRig(t, tc.policy, before)
			revision := rig.editor.Revision()
			stream, events := pasteThroughGio(rig, tc.data)
			wantEditorRejection(t, events, tc.want)
			rig.wantDraft(before)
			rig.wantShown(before.Text)
			if rig.editor.Revision() != revision || stream.closed != 1 {
				t.Fatalf("revision %d (was %d), stream closed %d times; want no edit and one close", rig.editor.Revision(), revision, stream.closed)
			}
			if got := rig.editor.TakeRejected(); got != tc.kept {
				t.Fatalf("recoverable candidate %q, want %q", got, tc.kept)
			}
		})
	}
}

func TestEditorPasteIsOneUndoableEdit(t *testing.T) {
	before := textinput.Draft{Text: "ab", Selection: textinput.Range{Start: 1, End: 2}}
	rig := newEditorRig(t, prompt(100), before)
	pasteThroughGio(rig, "x\r\ny\n")
	rig.wantDraft(textinput.Draft{Text: "ax\ny\n", Selection: caretAt(5)})
	rig.wantShown("ax\ny\n")
	rig.press("Z", key.ModShortcut)
	rig.frame()
	rig.wantDraft(before)
	rig.wantShown(before.Text)
}

func TestEditorDiscardsAPasteThatArrivesDuringComposition(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{Text: "a", Selection: caretAt(1)})
	rig.press("V", key.ModShortcut)
	rig.frame()
	rig.ime.windowsComposition("ㅎ")
	rig.frame()
	e, _ := pasteEvent("application/text", "pasted")
	rig.router.Queue(e)
	wantEditorRejection(t, rig.frame(), ErrPasteStale)
	rig.wantComposing(true)
	rig.wantShown("aㅎ")
	rig.wantDraft(textinput.Draft{Text: "a", Selection: caretAt(1)})
}

type scriptedClipboard struct {
	content ClipboardContent
	release chan struct{}
}

func (c *scriptedClipboard) ReadClipboard(ctx context.Context, kind PasteKind) (ClipboardContent, error) {
	if kind != PasteOrdinary {
		return ClipboardContent{}, ErrUnsupportedPaste
	}
	select {
	case <-c.release:
		return c.content, nil
	case <-ctx.Done():
		return ClipboardContent{}, ctx.Err()
	}
}

func pasteThroughReader(rig *editorRig, content ClipboardContent, between func()) []EditorEvent {
	reader := &scriptedClipboard{content: content, release: make(chan struct{})}
	rig.editor.Clipboard = func() ClipboardReader { return reader }
	return pasteFrom(rig, reader, between)
}

func pasteFrom(rig *editorRig, reader *scriptedClipboard, between func()) []EditorEvent {
	woke := make(chan struct{}, 1)
	rig.editor.Wake = func() { woke <- struct{}{} }
	rig.press("V", key.ModShortcut)
	rig.frame()
	between()
	close(reader.release)
	select {
	case <-woke:
	case <-time.After(10 * time.Second):
		rig.t.Fatal("Ctrl+V never read the native clipboard reader")
	}
	return rig.frame()
}

func TestEditorAsksForTheClipboardReaderAtPasteTime(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{})
	var current ClipboardReader
	rig.editor.Clipboard = func() ClipboardReader { return current }
	pasteThroughGio(rig, "g")
	rig.wantDraft(textinput.Draft{Text: "g", Selection: caretAt(1)})
	reader := &scriptedClipboard{content: ClipboardContent{Text: "n"}, release: make(chan struct{})}
	current = reader
	pasteFrom(rig, reader, func() {
		if rig.router.ClipboardRequested() {
			t.Fatal("Ctrl+V asked Gio for the clipboard although a native reader appeared")
		}
	})
	rig.wantDraft(textinput.Draft{Text: "gn", Selection: caretAt(2)})
}

func TestEditorNativePasteValidatesAndBindsItsTarget(t *testing.T) {
	single := newEditorRig(t, singleLinePolicy(100), textinput.Draft{})
	wantEditorRejection(t, pasteThroughReader(single, ClipboardContent{Text: "a\nb"}, func() {}), textinput.LineBreakNotAllowed)
	single.wantDraft(textinput.Draft{})
	if got := single.editor.TakeRejected(); got != "a\nb" {
		t.Fatalf("recoverable candidate %q, want the clipboard text", got)
	}
	wantEditorRejection(t, pasteThroughReader(single, ClipboardContent{Image: []byte("\x89PNG"), ImageType: "image/png"}, func() {}), ErrUnsupportedPaste)

	rig := newEditorRig(t, prompt(100), textinput.Draft{})
	stale := pasteThroughReader(rig, ClipboardContent{Text: "late"}, func() {
		rig.ime.insert("q")
		rig.frame()
	})
	wantEditorRejection(t, stale, ErrPasteStale)
	rig.wantDraft(textinput.Draft{Text: "q", Selection: caretAt(1)})
	pasteThroughReader(rig, ClipboardContent{Text: "z"}, func() {})
	rig.wantDraft(textinput.Draft{Text: "qz", Selection: caretAt(2)})
}

func TestEditorNativePasteLosesToSameFrameInput(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{Text: "a", Selection: caretAt(1)})
	stale := pasteThroughReader(rig, ClipboardContent{Text: "late"}, func() {
		rig.ime.insert("q")
	})
	wantEditorRejection(t, stale, ErrPasteStale)
	rig.wantDraft(textinput.Draft{Text: "aq", Selection: caretAt(2)})
}

func TestEditorFocusLossCancelsTheComposition(t *testing.T) {
	before := textinput.Draft{Text: "東京は晴れ", Selection: textinput.Range{Start: 9, End: 15}}
	rig := newEditorRig(t, prompt(100), before)
	rig.ime.windowsComposition("漢字")
	rig.frame()
	rig.wantComposing(true)
	rig.router.Source().Execute(key.FocusCmd{Tag: nil})
	rig.frame()
	rig.wantComposing(false)
	rig.wantDraft(before)
	rig.wantShown(before.Text)
}

func TestEditorShowsTheSoftKeyboardOnFocus(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{})
	if got := rig.router.TextInputState(); got != input.TextInputOpen {
		t.Fatalf("text input state %v after focus, want Open", got)
	}
}

func TestEditorRefusesProgrammaticEditsDuringComposition(t *testing.T) {
	before := textinput.Draft{Text: "ab", Selection: caretAt(2)}
	rig := newEditorRig(t, prompt(100), before)
	rig.ime.windowsComposition("ㅎ")
	rig.frame()
	edits := map[string]func() error{
		"replace":   func() error { return rig.editor.Replace(caretAt(0), "x") },
		"select":    func() error { return rig.editor.Select(caretAt(0)) },
		"set draft": func() error { return rig.editor.SetDraft(textinput.Draft{}) },
	}
	for name, edit := range edits {
		if err := edit(); !errors.Is(err, ErrComposing) {
			t.Fatalf("%s during composition: error %v, want ErrComposing", name, err)
		}
	}
	rig.wantComposing(true)
	rig.wantDraft(before)
	rig.wantShown("abㅎ")
}

func TestEditorProgrammaticReplaceUndoesInOneStepAndSetDraftClearsHistory(t *testing.T) {
	before := textinput.Draft{Text: "ab", Selection: caretAt(2)}
	rig := newEditorRig(t, prompt(100), before)
	if err := rig.editor.Replace(textinput.Range{Start: 0, End: 2}, "xyz"); err != nil {
		t.Fatal(err)
	}
	rig.wantDraft(textinput.Draft{Text: "xyz", Selection: caretAt(3)})
	rig.wantShown("xyz")
	rig.press("Z", key.ModShortcut)
	rig.frame()
	rig.wantDraft(before)
	adopted := textinput.Draft{Text: "q", Selection: caretAt(1)}
	if err := rig.editor.SetDraft(adopted); err != nil {
		t.Fatal(err)
	}
	rig.press("Z", key.ModShortcut|key.ModShift)
	rig.frame()
	rig.press("Z", key.ModShortcut)
	rig.frame()
	rig.wantDraft(adopted)
	rig.wantShown("q")
}

func TestEditorReportsEnterEscapeAndClaimedKeysOutsideComposition(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{Text: "ab", Selection: caretAt(2)})
	rig.editor.Claim(key.NameUpArrow)
	rig.frame()
	cases := []struct {
		name key.Name
		mods key.Modifiers
		want EditorEventKind
	}{
		{key.NameReturn, 0, EditorSubmit},
		{key.NameEnter, key.ModAlt, EditorSubmit},
		{key.NameEscape, 0, EditorCancel},
		{key.NameUpArrow, key.ModShift, EditorKey},
	}
	for _, tc := range cases {
		rig.press(tc.name, tc.mods)
		events := rig.frame()
		if len(events) != 1 || events[0] != (EditorEvent{Kind: tc.want, Name: tc.name, Modifiers: tc.mods}) {
			t.Fatalf("%s with %v produced %+v, want one %v", tc.name, tc.mods, events, tc.want)
		}
	}
	rig.press(key.NameReturn, key.ModShift)
	if events := rig.frame(); hasEditorEvent(events, EditorSubmit) {
		t.Fatalf("Shift+Return in a multiline field submitted: %v", editorEventKinds(events))
	}
	rig.wantDraft(textinput.Draft{Text: "ab\n", Selection: caretAt(3)})
	rig.editor.Claim()
	rig.frame()
	rig.press(key.NameUpArrow, 0)
	if events := rig.frame(); hasEditorEvent(events, EditorKey) {
		t.Fatalf("an unclaimed arrow was reported: %v", editorEventKinds(events))
	}

	single := newEditorRig(t, singleLinePolicy(100), textinput.Draft{})
	single.press(key.NameReturn, key.ModShift)
	events := single.frame()
	if len(events) != 1 || events[0] != (EditorEvent{Kind: EditorSubmit, Name: key.NameReturn, Modifiers: key.ModShift}) {
		t.Fatalf("Shift+Return in a single-line field produced %+v, want a submit", events)
	}
}

func TestEditorValidatesEachTypedEdit(t *testing.T) {
	rig := newEditorRig(t, singleLinePolicy(3), textinput.Draft{})
	revision := rig.editor.Revision()
	rig.ime.insert("a")
	if events := rig.frame(); !hasEditorEvent(events, EditorChange) || rig.editor.Revision() == revision {
		t.Fatalf("typing produced %v at revision %d, want a change and a new revision", editorEventKinds(events), rig.editor.Revision())
	}
	rig.wantDraft(textinput.Draft{Text: "a", Selection: caretAt(1)})
	rig.ime.insert("\x00")
	wantEditorRejection(t, rig.frame(), textinput.DisallowedControl)
	rig.wantDraft(textinput.Draft{Text: "a", Selection: caretAt(1)})
	rig.wantShown("a")
	rig.ime.insert("bc")
	rig.frame()
	rig.ime.insert("d")
	wantEditorRejection(t, rig.frame(), textinput.TextTooLarge)
	rig.wantDraft(textinput.Draft{Text: "abc", Selection: caretAt(3)})
	rig.wantShown("abc")
}

func TestEditorMaskedFieldKeepsTheSecretOffTheClipboard(t *testing.T) {
	rig := newEditorRig(t, singleLinePolicy(100), textinput.Draft{Text: "secret", Selection: textinput.Range{Start: 0, End: 6}})
	rig.editor.Mask = '•'
	rig.frame()
	for _, name := range []key.Name{"C", "X"} {
		rig.press(name, key.ModShortcut)
		rig.frame()
		if _, content, ok := rig.router.WriteClipboard(); ok {
			t.Fatalf("Ctrl+%s copied %q out of a masked field", name, content)
		}
	}
	rig.wantDraft(textinput.Draft{Text: "secret", Selection: textinput.Range{Start: 0, End: 6}})
	if err := rig.editor.SetDraft(textinput.Draft{}); err != nil {
		t.Fatal(err)
	}
	rig.press("Z", key.ModShortcut)
	rig.frame()
	rig.wantDraft(textinput.Draft{})
}
