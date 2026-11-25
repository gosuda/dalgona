//go:build gui

package platform

import (
	"context"
	"errors"
	"image"
	"runtime"
	"slices"
	"strings"
	"unicode/utf8"

	"gioui.org/font"
	"gioui.org/io/clipboard"
	"gioui.org/io/event"
	"gioui.org/io/key"
	"gioui.org/io/transfer"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/op/paint"
	"gioui.org/text"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

type EditorEventKind uint8

const (
	EditorChange EditorEventKind = iota + 1
	EditorSubmit
	EditorCancel
	EditorKey
	EditorRejected
)

type EditorEvent struct {
	Kind      EditorEventKind
	Name      key.Name
	Modifiers key.Modifiers
	Err       error
}

type EditorStyle struct {
	Shaper    *text.Shaper
	Font      font.Font
	Size      unit.Sp
	Text      op.CallOp
	Selection op.CallOp
}

const anyKeyModifier = key.ModCtrl | key.ModCommand | key.ModShift | key.ModAlt | key.ModSuper

const (
	editorUndoSteps = 1000
	editorUndoBytes = 32 << 20
)

var noRuneRange = key.Range{Start: -1, End: -1}

var imeOwnedKeys = []key.Name{
	key.NameDeleteBackward, key.NameDeleteForward, key.NameHome, key.NameEnd, key.NamePageUp, key.NamePageDown,
	key.NameLeftArrow, key.NameRightArrow, key.NameUpArrow, key.NameDownArrow, key.NameTab,
	"A", "C", "V", "X", "Z",
}

type Editor struct {
	Mask      rune
	Clipboard func() ClipboardReader
	Wake      func()

	widget        widget.Editor
	policy        textinput.Policy
	composer      *Composer
	view          string
	selection     key.Range
	composition   key.Range
	ime           imeMirror
	published     textinput.Draft
	revision      uint64
	rejected      string
	history       []transaction
	applied       int
	historyBytes  int
	widgetEdits   int
	widgetBytes   int
	claimed       []key.Name
	pending       []EditorEvent
	snippet       key.Range
	sentSnippet   key.Snippet
	snippetStale  bool
	paste         pasteRequest
	read          *clipboardRead
	regions       []widget.Region
	filters       []event.Filter
	batch         []event.Event
	unmarkCommits bool
}

type transaction struct {
	at       int
	removed  string
	inserted string
	before   textinput.Range
	after    textinput.Range
}

type pasteRequest struct {
	revision uint64
	waiting  bool
}

type clipboardRead struct {
	revision uint64
	result   chan clipboardResult
	cancel   context.CancelFunc
}

type clipboardResult struct {
	content ClipboardContent
	err     error
}

func NewEditor(policy textinput.Policy, draft textinput.Draft) (*Editor, error) {
	composer, err := NewComposer(policy, draft)
	if err != nil {
		return nil, err
	}
	e := &Editor{
		policy:        policy,
		composer:      composer,
		published:     draft,
		composition:   noRuneRange,
		unmarkCommits: runtime.GOOS == "darwin",
	}
	e.widget.SingleLine = policy.Lines == textinput.SingleLine
	e.showCanonical()
	return e, nil
}

func (e *Editor) Tag() event.Tag { return &e.widget }

func (e *Editor) Composing() bool { return e.composer.Composing() }

func (e *Editor) Draft() textinput.Draft { return e.composer.Draft() }

func (e *Editor) Revision() uint64 { return e.revision }

func (e *Editor) TakeRejected() string {
	candidate := e.rejected
	e.rejected = ""
	return candidate
}

func (e *Editor) Claim(names ...key.Name) {
	e.claimed = append(e.claimed[:0], names...)
}

func (e *Editor) Select(r textinput.Range) error {
	if err := e.composer.Select(r); err != nil {
		return err
	}
	e.showCanonical()
	e.publish()
	return nil
}

func (e *Editor) Replace(r textinput.Range, candidate string) error {
	if e.composer.Composing() {
		return ErrComposing
	}
	before := e.composer.Draft()
	if err := e.composer.Select(r); err != nil {
		return err
	}
	if err := e.composer.Replace(candidate); err != nil {
		e.composer.TakeRejected()
		e.rejected = candidate
		return errors.Join(err, e.composer.Select(before.Selection))
	}
	e.record(before, e.composer.Draft())
	e.showCanonical()
	e.publish()
	return nil
}

func (e *Editor) SetDraft(d textinput.Draft) error {
	if e.composer.Composing() {
		return ErrComposing
	}
	if err := e.adopt(d); err != nil {
		return err
	}
	clear(e.history)
	e.history, e.applied, e.historyBytes = e.history[:0], 0, 0
	return nil
}

func (e *Editor) Update(gtx layout.Context) (EditorEvent, bool) {
	if len(e.pending) == 0 {
		e.process(gtx)
	}
	if len(e.pending) == 0 {
		return EditorEvent{}, false
	}
	ev := e.pending[0]
	e.pending = e.pending[:copy(e.pending, e.pending[1:])]
	return ev, true
}

func (e *Editor) Layout(gtx layout.Context, style EditorStyle) layout.Dimensions {
	e.process(gtx)
	dims := e.widget.Layout(gtx, style.Shaper, style.Font, style.Size, style.Text, style.Selection)
	visible := image.Rectangle{Max: dims.Size}
	if gtx.Focused(e.Tag()) {
		e.publishSelection(gtx, visible)
		e.publishSnippet(gtx)
	}
	e.paintComposition(gtx, style.Text, visible)
	return dims
}

func (e *Editor) process(gtx layout.Context) {
	e.widget.Mask = e.Mask
	e.drain(gtx)
	if e.composer.Composing() {
		e.swallow(gtx)
	}
	e.runWidget(gtx)
	e.settle()
	e.receiveClipboard()
}

func (e *Editor) drain(gtx layout.Context) {
	filters := e.keyFilters()
	for {
		ev, ok := gtx.Event(filters...)
		if !ok {
			break
		}
		e.batch = append(e.batch, ev)
	}
	e.resetMirror()
	e.ime.ended = false
	for i, ev := range e.batch {
		e.handle(gtx, ev, e.batch[i+1:])
	}
	e.settle()
	clear(e.batch)
	e.batch = e.batch[:0]
}

func (e *Editor) keyFilters() []event.Filter {
	tag := e.Tag()
	e.filters = append(e.filters[:0],
		key.FocusFilter{Target: tag},
		PasteFilter(tag),
		key.Filter{Focus: tag, Name: key.NameReturn, Optional: anyKeyModifier},
		key.Filter{Focus: tag, Name: key.NameEnter, Optional: anyKeyModifier},
		key.Filter{Focus: tag, Name: key.NameEscape, Optional: anyKeyModifier},
		key.Filter{Focus: tag, Name: "V", Required: key.ModShortcut},
		key.Filter{Focus: tag, Name: "Z", Required: key.ModShortcut, Optional: key.ModShift},
	)
	e.filters = appendKeyFilters(e.filters, tag, e.claimed)
	if e.Mask != 0 {
		e.filters = append(e.filters,
			key.Filter{Focus: tag, Name: "C", Required: key.ModShortcut},
			key.Filter{Focus: tag, Name: "X", Required: key.ModShortcut},
		)
	}
	if e.composer.Composing() {
		e.filters = appendKeyFilters(e.filters, tag, imeOwnedKeys)
	}
	return e.filters
}

func appendKeyFilters(filters []event.Filter, tag event.Tag, names []key.Name) []event.Filter {
	for _, name := range names {
		filters = append(filters, key.Filter{Focus: tag, Name: name, Optional: anyKeyModifier})
	}
	return filters
}

func (e *Editor) swallow(gtx layout.Context) {
	e.filters = appendKeyFilters(e.filters[:0], e.Tag(), imeOwnedKeys)
	for {
		if _, ok := gtx.Event(e.filters...); !ok {
			return
		}
	}
}

func (e *Editor) handle(gtx layout.Context, ev event.Event, rest []event.Event) {
	switch ev := ev.(type) {
	case key.EditEvent:
		e.ime.edit(ev, nextComposition(rest))
	case key.CompositionEvent:
		e.mark(key.Range(ev))
	case key.SelectionEvent:
		e.ime.place(key.Range(ev))
	case key.SnippetEvent:
		e.snippet, e.snippetStale = key.Range(ev), true
	case key.FocusEvent:
		e.focus(gtx, ev)
	case key.Event:
		e.press(gtx, ev)
	case transfer.DataEvent:
		e.receivePaste(ev)
	}
}

func nextComposition(rest []event.Event) key.Range {
	for _, ev := range rest {
		switch ev := ev.(type) {
		case key.CompositionEvent:
			return key.Range(ev)
		case key.EditEvent, key.SelectionEvent:
			return noRuneRange
		}
	}
	return noRuneRange
}

func (e *Editor) mark(r key.Range) {
	if r.Start >= 0 {
		e.ime.mark(r)
		return
	}
	m := &e.ime
	m.dirty = true
	m.ended = m.ended || m.active
	m.active = false
	if m.region.Start < 0 || m.region.Start == m.region.End {
		return
	}
	if e.unmarkCommits {
		m.region, m.anchor = noRuneRange, ""
		return
	}
	e.settle()
}

func (e *Editor) focus(gtx layout.Context, ev key.FocusEvent) {
	e.sentSnippet, e.snippetStale = key.Snippet{Range: noRuneRange}, true
	if ev.Focus {
		gtx.Execute(key.SoftKeyboardCmd{Show: true})
		return
	}
	if e.ime.region.Start >= 0 {
		e.ime.active, e.ime.dirty = false, true
		e.settle()
	}
}

func (e *Editor) press(gtx layout.Context, k key.Event) {
	if k.State != key.Press || e.ime.active {
		return
	}
	if e.ime.ended {
		e.ime.ended = false
		return
	}
	e.settle()
	switch {
	case k.Name == key.NameReturn || k.Name == key.NameEnter:
		e.enter(k)
	case k.Name == key.NameEscape:
		e.emit(EditorEvent{Kind: EditorCancel, Name: k.Name, Modifiers: k.Modifiers})
	case slices.Contains(e.claimed, k.Name):
		e.emit(EditorEvent{Kind: EditorKey, Name: k.Name, Modifiers: k.Modifiers})
	case k.Modifiers.Contain(key.ModShortcut):
		e.shortcut(gtx, k)
	}
}

func (e *Editor) enter(k key.Event) {
	if e.policy.Lines == textinput.MultiLine && k.Modifiers == key.ModShift {
		e.insert("\n")
		return
	}
	e.emit(EditorEvent{Kind: EditorSubmit, Name: k.Name, Modifiers: k.Modifiers})
}

func (e *Editor) shortcut(gtx layout.Context, k key.Event) {
	switch {
	case k.Name == "V":
		e.requestPaste(gtx)
	case k.Name == "Z" && k.Modifiers.Contain(key.ModShift):
		e.redo()
	case k.Name == "Z":
		e.undo()
	}
}

func (e *Editor) emit(ev EditorEvent) {
	e.pending = append(e.pending, ev)
}

func (e *Editor) refuse(err error, candidate string) {
	e.composer.TakeRejected()
	e.rejected = candidate
	e.emit(EditorEvent{Kind: EditorRejected, Err: err})
}

func (e *Editor) publish() {
	d := e.composer.Draft()
	if d == e.published {
		return
	}
	e.published = d
	e.revision++
	e.emit(EditorEvent{Kind: EditorChange})
}

func (e *Editor) resetMirror() {
	e.ime = imeMirror{
		text:      e.view,
		selection: e.selection,
		region:    e.composition,
		active:    e.composer.Composing(),
		ended:     e.ime.ended,
	}
	if e.ime.active {
		d := e.composer.Draft()
		e.ime.anchor = d.Text[d.Selection.Start:d.Selection.End]
	}
}

func (e *Editor) settle() {
	if !e.ime.dirty {
		return
	}
	before := e.composer.Draft()
	candidate, at := e.ime.canonical()
	e.composer.Cancel()
	accepted := candidate == before.Text || e.commitDiff(before, candidate)
	if accepted && e.composer.Draft().Text == candidate {
		e.adoptMirror(at)
	} else {
		e.showCanonical()
	}
	e.publish()
	e.resetMirror()
}

func (e *Editor) commitDiff(before textinput.Draft, candidate string) bool {
	r, inserted, err := editedRange(before.Text, candidate)
	if err == nil {
		err = e.composer.Select(r)
	}
	if err == nil {
		err = e.composer.Replace(inserted)
	}
	if err != nil {
		e.refuse(errors.Join(err, e.composer.Select(before.Selection)), inserted)
		return false
	}
	e.record(before, e.composer.Draft())
	return true
}

func (e *Editor) adoptMirror(at int) {
	m := &e.ime
	if m.region.Start < 0 {
		e.adoptSelection(m.text, m.selection)
		return
	}
	err := e.composer.Select(textinput.Range{Start: at, End: at + len(m.anchor)})
	if !m.active {
		e.showCanonical()
		return
	}
	if err != nil {
		e.refuse(err, m.slice(m.region))
		e.showCanonical()
		return
	}
	e.composer.Compose(m.slice(m.region))
	e.composition = m.region
	e.showView(m.text, m.selection)
}

func (e *Editor) adoptSelection(text string, sel key.Range) {
	if e.composer.Select(runeRangeBytes(text, sel)) != nil {
		e.showCanonical()
		return
	}
	e.composition = noRuneRange
	e.showView(text, sel)
}

func (e *Editor) showCanonical() {
	d := e.composer.Draft()
	e.composition = noRuneRange
	e.showView(d.Text, key.Range{
		Start: utf8.RuneCountInString(d.Text[:d.Selection.End]),
		End:   utf8.RuneCountInString(d.Text[:d.Selection.Start]),
	})
}

func (e *Editor) showView(view string, sel key.Range) {
	if view != e.view {
		e.setWidgetText(view)
		e.view, e.snippetStale = view, true
	}
	e.widget.SetCaret(sel.Start, sel.End)
	start, end := e.widget.Selection()
	e.selection = key.Range{Start: start, End: end}
	e.boundWidgetHistory()
}

func (e *Editor) setWidgetText(next string) {
	current := e.view
	p := sharedPrefixLen(current, next)
	s := sharedSuffixLen(current[p:], next[p:])
	start := utf8.RuneCountInString(current[:p])
	e.widget.SetCaret(start, start+utf8.RuneCountInString(current[p:len(current)-s]))
	e.widget.Insert(next[p : len(next)-s])
	e.widgetEdits++
	e.widgetBytes += len(current) + len(next) - 2*(p+s)
	if shown := e.widget.Text(); shown != next {
		e.widget.SetText(next)
		e.widgetEdits++
		e.widgetBytes += len(shown) + len(next)
	}
}

func (e *Editor) boundWidgetHistory() {
	if e.widgetEdits < editorUndoSteps && e.widgetBytes < editorUndoBytes || e.composer.Composing() {
		return
	}
	caret, anchor := e.widget.Selection()
	e.widget = widget.Editor{SingleLine: e.widget.SingleLine, Mask: e.widget.Mask}
	e.widget.SetText(e.view)
	e.widget.SetCaret(caret, anchor)
	e.widgetEdits, e.widgetBytes, e.snippetStale = 0, 0, true
}

func (e *Editor) runWidget(gtx layout.Context) {
	edits := 0
	for {
		ev, ok := e.widget.Update(gtx)
		if !ok {
			break
		}
		if _, edited := ev.(widget.ChangeEvent); edited {
			edits++
		}
	}
	if edits > 0 {
		e.widgetEdits += edits
		e.adoptWidgetText()
	}
	e.adoptWidgetSelection()
	e.boundWidgetHistory()
}

func (e *Editor) adoptWidgetText() {
	shown := e.widget.Text()
	if shown == e.view {
		return
	}
	held := e.view
	p := sharedPrefixLen(held, shown)
	e.widgetBytes += len(held) + len(shown) - 2*(p+sharedSuffixLen(held[p:], shown[p:]))
	e.view, e.snippetStale = shown, true
	if e.composer.Composing() {
		e.showView(held, e.selection)
		return
	}
	if e.commitDiff(e.composer.Draft(), shown) && e.composer.Draft().Text == shown {
		e.selection = noRuneRange
	} else {
		e.showCanonical()
	}
	e.publish()
}

func (e *Editor) adoptWidgetSelection() {
	start, end := e.widget.Selection()
	sel := key.Range{Start: start, End: end}
	if sel == e.selection || e.composer.Composing() {
		return
	}
	e.selection = sel
	if e.composer.Select(runeRangeBytes(e.view, sel)) != nil {
		e.showCanonical()
	}
	e.publish()
}

func (e *Editor) insert(candidate string) {
	if err := e.Replace(e.composer.Draft().Selection, candidate); err != nil {
		e.emit(EditorEvent{Kind: EditorRejected, Err: err})
	}
}

func (e *Editor) record(before, after textinput.Draft) {
	p := sharedPrefixLen(before.Text, after.Text)
	s := sharedSuffixLen(before.Text[p:], after.Text[p:])
	for _, undone := range e.history[e.applied:] {
		e.historyBytes -= undone.size()
	}
	clear(e.history[e.applied:])
	e.history = append(e.history[:e.applied], transaction{
		at:       p,
		removed:  strings.Clone(before.Text[p : len(before.Text)-s]),
		inserted: strings.Clone(after.Text[p : len(after.Text)-s]),
		before:   before.Selection,
		after:    after.Selection,
	})
	e.historyBytes += e.history[len(e.history)-1].size()
	oldest := 0
	for kept := len(e.history); kept > editorUndoSteps || kept > 1 && e.historyBytes > editorUndoBytes; kept-- {
		e.historyBytes -= e.history[oldest].size()
		oldest++
	}
	e.history = slices.Delete(e.history, 0, oldest)
	e.applied = len(e.history)
}

func (t transaction) size() int {
	return len(t.removed) + len(t.inserted)
}

func (e *Editor) undo() {
	if e.applied == 0 {
		return
	}
	tx := e.history[e.applied-1]
	text := e.composer.Draft().Text
	if e.restore(textinput.Draft{Text: text[:tx.at] + tx.removed + text[tx.at+len(tx.inserted):], Selection: tx.before}) {
		e.applied--
	}
}

func (e *Editor) redo() {
	if e.applied == len(e.history) {
		return
	}
	tx := e.history[e.applied]
	text := e.composer.Draft().Text
	if e.restore(textinput.Draft{Text: text[:tx.at] + tx.inserted + text[tx.at+len(tx.removed):], Selection: tx.after}) {
		e.applied++
	}
}

func (e *Editor) restore(d textinput.Draft) bool {
	if err := e.adopt(d); err != nil {
		e.emit(EditorEvent{Kind: EditorRejected, Err: err})
		return false
	}
	return true
}

func (e *Editor) adopt(d textinput.Draft) error {
	composer, err := NewComposer(e.policy, d)
	if err != nil {
		return err
	}
	e.composer = composer
	e.showCanonical()
	e.publish()
	return nil
}

func (e *Editor) requestPaste(gtx layout.Context) {
	var reader ClipboardReader
	if e.Clipboard != nil {
		reader = e.Clipboard()
	}
	if reader != nil {
		e.startClipboardRead(reader)
		return
	}
	e.paste = pasteRequest{revision: e.revision, waiting: true}
	gtx.Execute(clipboard.ReadCmd{Tag: e.Tag()})
}

func (e *Editor) receivePaste(ev transfer.DataEvent) {
	if !e.paste.waiting {
		return
	}
	e.paste.waiting = false
	text, err := ReadPaste(ev)
	e.settle()
	e.applyPaste(e.paste.revision, text, err)
}

func (e *Editor) startClipboardRead(reader ClipboardReader) {
	e.stopClipboardRead()
	ctx, cancel := context.WithCancel(context.Background())
	read := &clipboardRead{revision: e.revision, result: make(chan clipboardResult, 1), cancel: cancel}
	e.read = read
	go readNativeClipboard(ctx, reader, e.Wake, read.result)
}

func readNativeClipboard(ctx context.Context, reader ClipboardReader, wake func(), result chan<- clipboardResult) {
	content, err := reader.ReadClipboard(ctx, PasteOrdinary)
	result <- clipboardResult{content: content, err: err}
	if wake != nil {
		wake()
	}
}

func (e *Editor) stopClipboardRead() {
	if e.read != nil {
		e.read.cancel()
		e.read = nil
	}
}

func (e *Editor) receiveClipboard() {
	if e.read == nil {
		return
	}
	select {
	case res := <-e.read.result:
		revision := e.read.revision
		e.stopClipboardRead()
		text, err := nativePasteText(res)
		e.applyPaste(revision, text, err)
	default:
	}
}

func nativePasteText(res clipboardResult) (string, error) {
	if res.err != nil {
		return "", res.err
	}
	if len(res.content.Image) > 0 || len(res.content.Files) > 0 {
		return "", ErrUnsupportedPaste
	}
	return res.content.Text, nil
}

func (e *Editor) applyPaste(revision uint64, text string, err error) {
	switch {
	case err != nil:
		e.emit(EditorEvent{Kind: EditorRejected, Err: err})
	case e.composer.Composing() || revision != e.revision:
		e.emit(EditorEvent{Kind: EditorRejected, Err: ErrPasteStale})
	default:
		e.insert(text)
	}
}

func (e *Editor) publishSelection(gtx layout.Context, visible image.Rectangle) {
	start, end := e.widget.Selection()
	gtx.Execute(key.SelectionCmd{
		Tag:               e.Tag(),
		Range:             key.Range{Start: min(start, end), End: max(start, end)},
		Caret:             e.caret(start),
		CompositionBounds: e.compositionBounds(visible),
	})
}

func (e *Editor) caret(at int) key.Caret {
	pos := e.widget.CaretCoords()
	e.regions = e.widget.Regions(at, at, e.regions)
	if len(e.regions) == 0 {
		return key.Caret{Pos: pos}
	}
	bounds := e.regions[0].Bounds
	return key.Caret{Pos: pos, Ascent: pos.Y - float32(bounds.Min.Y), Descent: float32(bounds.Max.Y) - pos.Y}
}

func (e *Editor) compositionRegions() []widget.Region {
	c := e.composition
	if c.Start < 0 || c.Start == c.End {
		return nil
	}
	e.regions = e.widget.Regions(c.Start, c.End, e.regions)
	return e.regions
}

func (e *Editor) compositionBounds(visible image.Rectangle) image.Rectangle {
	var bounds image.Rectangle
	for _, region := range e.compositionRegions() {
		bounds = bounds.Union(region.Bounds.Intersect(visible))
	}
	return bounds
}

func (e *Editor) paintComposition(gtx layout.Context, material op.CallOp, visible image.Rectangle) {
	thickness := max(gtx.Dp(unit.Dp(1)), 1)
	for _, region := range e.compositionRegions() {
		y := region.Bounds.Max.Y - max(region.Baseline/3, thickness)
		underline := image.Rect(region.Bounds.Min.X, y, region.Bounds.Max.X, y+thickness).Intersect(visible)
		if underline.Empty() {
			continue
		}
		stack := clip.Rect(underline).Push(gtx.Ops)
		material.Add(gtx.Ops)
		paint.PaintOp{}.Add(gtx.Ops)
		stack.Pop()
	}
}

func (e *Editor) publishSnippet(gtx layout.Context) {
	if !e.snippetStale {
		return
	}
	e.snippetStale = false
	snippet := editorSnippet(e.view, e.snippet)
	if snippet == e.sentSnippet {
		return
	}
	e.sentSnippet = snippet
	gtx.Execute(key.SnippetCmd{Tag: e.Tag(), Snippet: snippet})
}

func editorSnippet(text string, r key.Range) key.Snippet {
	r = clampRuneRange(r, utf8.RuneCountInString(text))
	start, end := runeSpanBytes(text, r)
	return key.Snippet{Range: r, Text: text[start:end]}
}

type editKind uint8

const (
	plainEdit editKind = iota
	preeditEdit
	clearEdit
	commitEdit
)

type imeMirror struct {
	text      string
	selection key.Range
	region    key.Range
	anchor    string
	active    bool
	dirty     bool
	ended     bool
}

func (m *imeMirror) edit(ev key.EditEvent, next key.Range) {
	m.dirty = true
	r := clampRuneRange(ev.Range, utf8.RuneCountInString(m.text))
	inserted := key.Range{Start: r.Start, End: r.Start + utf8.RuneCountInString(ev.Text)}
	kind := m.classify(r, inserted, next)
	if kind != plainEdit {
		m.enclose(r)
	}
	m.splice(r, ev.Text, inserted.End)
	switch kind {
	case commitEdit:
		m.region, m.anchor, m.active, m.ended = noRuneRange, "", false, true
	case plainEdit:
		m.ended = false
	case preeditEdit, clearEdit:
	}
}

func (m *imeMirror) classify(r, inserted, next key.Range) editKind {
	switch {
	case next.Start >= 0 && rangesTouch(next, inserted):
		return preeditEdit
	case m.region.Start < 0 || !rangesTouch(r, m.region):
		return plainEdit
	case inserted.Start == inserted.End && rangeWithin(r, m.region):
		return clearEdit
	}
	return commitEdit
}

func (m *imeMirror) mark(r key.Range) {
	m.dirty = true
	m.enclose(clampRuneRange(r, utf8.RuneCountInString(m.text)))
	m.active = true
}

func (m *imeMirror) place(r key.Range) {
	m.dirty = true
	m.selection = r
}

func (m *imeMirror) enclose(r key.Range) {
	if m.region.Start < 0 {
		m.region, m.anchor = r, m.slice(r)
		return
	}
	lo, hi := min(r.Start, m.region.Start), max(r.End, m.region.End)
	m.anchor = m.slice(key.Range{Start: lo, End: m.region.Start}) + m.anchor + m.slice(key.Range{Start: m.region.End, End: hi})
	m.region = key.Range{Start: lo, End: hi}
}

func (m *imeMirror) splice(r key.Range, replacement string, newEnd int) {
	start, end := runeSpanBytes(m.text, r)
	m.text = m.text[:start] + replacement + m.text[end:]
	m.selection = key.Range{Start: shiftRuneOffset(m.selection.Start, r, newEnd), End: shiftRuneOffset(m.selection.End, r, newEnd)}
	delta := newEnd - r.End
	switch {
	case m.region.Start < 0:
	case rangeWithin(r, m.region):
		m.region.End += delta
	case r.End <= m.region.Start:
		m.region.Start += delta
		m.region.End += delta
	}
}

func (m *imeMirror) slice(r key.Range) string {
	start, end := runeSpanBytes(m.text, r)
	return m.text[start:end]
}

func (m *imeMirror) canonical() (string, int) {
	if m.region.Start < 0 {
		return m.text, 0
	}
	start, end := runeSpanBytes(m.text, m.region)
	return m.text[:start] + m.anchor + m.text[end:], start
}

func shiftRuneOffset(pos int, r key.Range, newEnd int) int {
	switch {
	case newEnd < pos && pos <= r.End:
		return newEnd
	case r.End < pos:
		return pos + newEnd - r.End
	}
	return pos
}

func rangesTouch(a, b key.Range) bool {
	return a.Start <= b.End && b.Start <= a.End
}

func rangeWithin(inner, outer key.Range) bool {
	return outer.Start <= inner.Start && inner.End <= outer.End
}

func clampRuneRange(r key.Range, length int) key.Range {
	if r.Start > r.End {
		r.Start, r.End = r.End, r.Start
	}
	return key.Range{Start: min(max(r.Start, 0), length), End: min(max(r.End, 0), length)}
}

func runeSpanBytes(s string, r key.Range) (start, end int) {
	start = runeByteOffset(s, r.Start)
	return start, start + runeByteOffset(s[start:], r.End-r.Start)
}

func runeRangeBytes(s string, r key.Range) textinput.Range {
	start, end := runeSpanBytes(s, clampRuneRange(r, utf8.RuneCountInString(s)))
	return textinput.Range{Start: start, End: end}
}

func runeByteOffset(s string, runes int) int {
	for i := range s {
		if runes <= 0 {
			return i
		}
		runes--
	}
	return len(s)
}

func sharedPrefixLen(a, b string) int {
	n := min(len(a), len(b))
	i := 0
	for i < n && a[i] == b[i] {
		i++
	}
	for i > 0 && i < len(a) && !utf8.RuneStart(a[i]) {
		i--
	}
	return i
}

func sharedSuffixLen(a, b string) int {
	n := min(len(a), len(b))
	i := 0
	for i < n && a[len(a)-1-i] == b[len(b)-1-i] {
		i++
	}
	for i > 0 && !utf8.RuneStart(a[len(a)-i]) {
		i--
	}
	return i
}

func editedRange(old, next string) (textinput.Range, string, error) {
	p := sharedPrefixLen(old, next)
	s := sharedSuffixLen(old[p:], next[p:])
	r, err := textinput.PaintRange(old, textinput.Range{Start: p, End: len(old) - s})
	if err != nil {
		return r, "", err
	}
	return r, next[r.Start : len(next)-len(old)+r.End], nil
}
