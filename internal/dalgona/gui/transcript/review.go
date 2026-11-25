//go:build gui

package transcript

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"image"
	"io"
	"math"
	"os"
	"slices"
	"strconv"
	"strings"
	"sync"
	"unicode/utf8"

	giofont "gioui.org/font"
	"gioui.org/io/clipboard"
	"gioui.org/io/event"
	"gioui.org/io/key"
	"gioui.org/io/pointer"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/op/paint"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	viewerWindow = 8 << 20
	itemField    = "\x00field\x00"
)

var (
	errStillLoading = errors.New("the full text is still loading; try again when every section has loaded")
	errNoSave       = errors.New("saving files is unavailable in this window")
)

type viewField struct {
	id       document.Identity
	title    string
	meta     string
	thread   state.ThreadKey
	item     state.ItemKey
	ref      rpc.ContentReference
	live     liveTail
	hasRef   bool
	suffix   string
	convert  func(string) string
	pretty   bool
	format   document.Format
	total    uint64
	base     uint64
	next     uint64
	data     []byte
	err      error
	asked    uint64
	more     widget.Clickable
	retry    widget.Clickable
	restart  widget.Clickable
	pull     bool
	shown    uint64
	deferred bool
}

func (f *viewField) complete() bool { return f.err == nil && (!f.hasRef || f.next == f.total) }

func (f *viewField) text() string {
	data := f.data
	if !f.complete() {
		data = validPrefix(data)
	}
	if f.base > 0 {
		if cut := bytes.IndexByte(data, '\n'); cut >= 0 {
			data = data[cut+1:]
		}
	}
	text := strings.ToValidUTF8(string(data), "\uFFFD")
	if f.complete() {
		text += f.suffix
		if f.convert != nil {
			text = f.convert(text)
		}
	}
	return text
}

func (f *viewField) displayText(l *loader) (string, error) {
	text := f.text()
	if f.pretty && f.complete() {
		return prettyJSON(text, 0, l)
	}
	return text, nil
}

func (f *viewField) state() string {
	switch {
	case !f.hasRef && f.suffix == "":
		return "No content was recorded for this item."
	case f.hasRef && f.total == 0 && f.suffix == "":
		return "Empty output."
	case f.err != nil:
		return "Loading stopped at byte " + strconv.FormatUint(f.next, 10) + ": " + f.err.Error()
	case f.base > 0:
		return "Showing from byte " + strconv.FormatUint(f.base, 10) + " of " + strconv.FormatUint(f.total, 10) + "; earlier output was released from memory."
	case !f.complete():
		return "Showing " + byteSize(f.next) + " of " + byteSize(f.total) + "."
	}
	return ""
}

type job struct {
	cancel context.CancelFunc
	name   string
	gen    uint64
}

type reviewer struct {
	t       *Transcript
	opened  bool
	opener  event.Tag
	mount   *reviewMount
	thread  state.ThreadKey
	link    any
	fields  []*viewField
	pane    *pane
	focus   struct{ _ byte }
	bridge  bridge
	close   widget.Clickable
	copyAll widget.Clickable
	export  widget.Clickable
	search  widget.Clickable
	find    *platform.Editor
	tags    []event.Tag
	refocus bool
	mu      sync.Mutex
	jobs    []*job
	gen     uint64
	status  string
	copied  *reviewCopy
	spill   *pane
	copyErr error
	match   *matchSpot
}

type reviewMount struct {
	target   *row
	revision string
	frame    uint64
}

type matchSpot struct {
	field  int
	offset uint64
	length int
}

func (r *reviewer) active() bool { return r != nil && r.opened }

func (r *reviewer) busy() bool {
	if r == nil {
		return false
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	return len(r.jobs) > 0 || r.copied != nil || r.copyErr != nil || r.bridge.waiting()
}

func (t *Transcript) openReview(gtx layout.Context, target *row, opener event.Tag) {
	find, err := platform.NewEditor(textinput.Policy{
		Lines: textinput.SingleLine,
		Limit: textinput.Limit{Unit: textinput.Bytes, Max: document.PageBytes},
	}, textinput.Draft{})
	if err != nil {
		t.notice = "Full text could not open: " + err.Error()
		return
	}
	if t.review == nil {
		t.review = &reviewer{t: t}
	}
	r := t.review
	r.cancelJobs()
	if r.spill != nil {
		t.pendingCopy = nil
	}
	r.mu.Lock()
	r.match, r.status = nil, ""
	r.mu.Unlock()
	r.opened, r.opener, r.thread = true, opener, target.entry.Thread
	r.spill = nil
	r.find, r.refocus = find, false
	r.pane = newPane(target.entry.Thread)
	r.pane.pinTop = true
	r.fields = nil
	members := []Entry{target.entry}
	if target.entry.Kind == KindGroup {
		members = target.entry.Members
	}
	for _, m := range members {
		r.fields = append(r.fields, t.viewFields(m)...)
	}
	r.mountReview(gtx, target)
}

func (r *reviewer) mountReview(gtx layout.Context, target *row) {
	r.link = nil
	if v := r.t.env.View; v != nil {
		if tv, ok := v.Thread(r.thread); ok {
			r.link = tv.Link
		}
	}
	r.mount = nil
	if p := r.t.panes[r.thread]; p != nil && r.opener == &target.open {
		r.mount = &reviewMount{target: target, revision: target.entry.Item.Header.ItemRevision, frame: p.frame}
		gtx.Execute(op.InvalidateCmd{})
		return
	}
	gtx.Execute(key.FocusCmd{Tag: &r.focus})
}

func (r *reviewer) shut(gtx layout.Context) {
	r.cancelJobs()
	if pending := r.t.pendingCopy; pending != nil && pending.pane == r.pane {
		r.t.pendingCopy = nil
	}
	r.opened, r.refocus = false, true
	r.mount = nil
	r.fields, r.pane, r.find = nil, nil, nil
	clear(r.tags)
	r.tags = r.tags[:0]
	gtx.Execute(key.FocusCmd{Tag: r.opener})
	gtx.Execute(op.InvalidateCmd{})
}

func (r *reviewer) cancelJobs() {
	r.mu.Lock()
	r.gen++
	for _, j := range r.jobs {
		j.cancel()
	}
	r.jobs = nil
	r.copyErr = nil
	if r.copied != nil {
		_ = r.copied.Close()
		r.copied = nil
	}
	r.mu.Unlock()
	r.bridge.abort()
}

func (r *reviewer) restoreFocus(gtx layout.Context, p *pane) {
	if r == nil || !r.refocus || !gtx.Enabled() {
		return
	}
	tag := r.t.FocusTag()
	if p != nil && p.key == r.thread {
		for _, placed := range p.placed {
			if placed.last && &placed.row.open == r.opener {
				tag = r.opener
				break
			}
		}
	}
	r.refocus, r.opener = false, nil
	gtx.Execute(key.FocusCmd{Tag: tag})
}

type fieldSpec struct {
	name    string
	label   string
	convert func(string) string
	pretty  bool
	format  document.Format
}

func (t *Transcript) viewFields(e Entry) []*viewField {
	title, meta := t.sectionTitle(e)
	var out []*viewField
	for _, spec := range spillSpecs(e, fieldSpecs(e)) {
		f := &viewField{
			id:     identity(e.Thread, e.Turn, e.Item.Key.Item+itemField+spec.name),
			title:  title + " · " + spec.label,
			meta:   meta,
			thread: e.Thread,
			item:   e.Item.Key,
			format: spec.format,
		}
		f.bindSource(e.Item, spec)
		if !f.hasRef && spec.name == "error" && e.Facts.errText != "" {
			f.suffix = e.Facts.errText
		}
		out = append(out, f)
	}
	return out
}

func (f *viewField) bindSource(item state.ItemView, spec fieldSpec) {
	if ref, ok := refNamed(item.Header, spec.name); ok {
		f.ref, f.hasRef = ref, true
		f.total, f.err = rpc.ParseDecimal(ref.Total)
	}
	if item.Field == spec.name && len(item.Tail) > 0 {
		live := captureTail(item)
		if live.size > math.MaxUint64-f.total {
			f.err = errMismatch
			return
		}
		f.live, f.total, f.hasRef = live, f.total+live.size, true
	}
	if f.hasRef && f.total <= inlineLimit {
		f.convert, f.pretty = spec.convert, spec.pretty
	}
}

func (f *viewField) recordedBytes() uint64 { return f.total - f.live.size }

func (t *Transcript) sectionTitle(e Entry) (string, string) {
	meta := strings.Join(nonEmpty([]string{stateWord(t.displayStatus(e)), exitWord(e), durationWord(e)}), " · ")
	if !toolKind(e.Kind) {
		meta = ""
	}
	switch e.Kind {
	case KindCommand:
		return "Command", meta
	case KindThinking:
		return "Thinking", meta
	case KindUser, KindAssistant, KindFileChange, KindTool, KindAdvisor, KindPlanItem, KindCompaction, KindOther, KindGroup:
	}
	return cmpOr(headerTitle(&row{entry: e, synth: synthNone}), kindLabel(e.Item.Header.Type)), meta
}

func fieldSpecs(e Entry) []fieldSpec {
	code := document.FormatCode
	switch e.Kind {
	case KindUser:
		return []fieldSpec{{name: "text", label: "Message", format: document.FormatNotice}}
	case KindAssistant:
		return []fieldSpec{{name: "text", label: "Message", format: document.FormatMarkdown}}
	case KindThinking:
		return []fieldSpec{{name: "content", label: "Reasoning", format: document.FormatNotice}}
	case KindCommand:
		return []fieldSpec{{name: "command", label: "Command", format: code}, {name: "aggregatedOutput", label: "Output", format: code}}
	case KindFileChange:
		return []fieldSpec{{name: "changes", label: "Diffs", format: document.FormatPatch, convert: fullDiff}}
	case KindTool, KindAdvisor:
		return toolSpecs(e)
	case KindPlanItem, KindCompaction, KindOther, KindGroup:
	}
	specs := make([]fieldSpec, 0, len(e.Item.Header.Contents))
	for _, ref := range e.Item.Header.Contents {
		specs = append(specs, fieldSpec{name: ref.Field, label: ref.Field, format: document.FormatNotice})
	}
	return specs
}

func toolSpecs(e Entry) []fieldSpec {
	code := document.FormatCode
	args := fieldSpec{name: "arguments", label: "Arguments", format: code, pretty: true}
	if e.Facts.tool == toolEval {
		args.convert, args.pretty = evalCode, false
	}
	switch e.Item.Header.Type {
	case rpc.KindMCP:
		return []fieldSpec{args, {name: "result", label: "Result", format: code, convert: contentText}, {name: "error", label: "Error", format: code}}
	case rpc.KindDynamic:
		return []fieldSpec{args, {name: "contentItems", label: "Output", format: code, convert: itemsText}}
	}
	return []fieldSpec{args, {name: "output", label: "Output", format: code, convert: tailored}}
}

var outputFields = map[string]bool{"aggregatedOutput": true, "output": true, "result": true, "contentItems": true}

func spillSpecs(e Entry, specs []fieldSpec) []fieldSpec {
	if _, ok := refNamed(e.Item.Header, fieldSpill); !ok {
		return specs
	}
	for i, spec := range specs {
		if outputFields[spec.name] {
			specs[i] = fieldSpec{name: fieldSpill, label: spec.label, format: spec.format}
		}
	}
	return specs
}

func contentText(raw string) string {
	return tailored(partsText(fieldText{text: raw, ready: true}, "content").text)
}

func itemsText(raw string) string {
	return tailored(partsText(fieldText{text: raw, ready: true}, "").text)
}

func fullDiff(raw string) string {
	changes, err := parseChanges([]byte(raw))
	if err != nil {
		return raw
	}
	source, _ := presentChanges(changes, diffView{}).source()
	return source
}

func (r *reviewer) step(v *state.View, epoch uint64, submit func(state.Intent)) {
	tv, ok := v.Thread(r.thread)
	if !ok {
		return
	}
	for _, f := range r.fields {
		if !f.hasRef || f.complete() || f.err != nil {
			continue
		}
		if f.live.size > 0 && f.next >= f.recordedBytes() {
			r.stepLive(tv, f)
		} else {
			r.stepRecorded(tv, f, epoch, submit)
		}
	}
}

func (r *reviewer) stepRecorded(tv *state.ThreadView, f *viewField, epoch uint64, submit func(state.Intent)) {
	key := state.ContentKey{Item: f.item, ContentID: f.ref.ContentID, Offset: f.next}
	if page, ok := tv.Content(key); ok {
		f.receive(key, page, submit)
		return
	}
	recent := f.asked != 0 && epoch-f.asked < settleEpochs
	if !recent && f.wanted(r) {
		f.asked, f.pull = epoch, false
		submit(state.ReadContent{Thread: f.thread, Item: key.Item, ContentID: key.ContentID, Offset: key.Offset})
	}
}

func (r *reviewer) stepLive(tv *state.ThreadView, f *viewField) {
	if !f.wanted(r) {
		return
	}
	f.pull = false
	data, err := f.live.page(tv, f.item, f.next-f.recordedBytes(), inlineLimit)
	if err != nil {
		f.err = err
		return
	}
	f.accept(state.ContentPage{Data: data, Next: f.next + uint64(len(data)), Total: f.total})
}

func (f *viewField) receive(key state.ContentKey, page state.ContentPage, submit func(state.Intent)) {
	if page.Loading {
		return
	}
	submit(state.ReleaseContentPage{Thread: f.thread, Key: key})
	switch {
	case page.Err == nil:
		if page.Total != f.recordedBytes() || page.Next > page.Total {
			f.err = errMismatch
			return
		}
		page.Total = f.total
		f.accept(page)
	case !f.pull:
		f.err = page.Err
	default:
		f.asked = 0
	}
}

func (f *viewField) wanted(r *reviewer) bool {
	if f.pull || f.next == f.base && !f.deferred {
		return true
	}
	for _, pl := range r.pane.placed {
		if pl.row.id == f.id && (f.deferred || f.shown == f.next && pl.last) {
			return true
		}
	}
	return false
}

func (f *viewField) accept(page state.ContentPage) {
	f.asked = 0
	if page.Next <= f.next || page.Next > f.total || page.Total != f.total {
		f.err = errMismatch
		return
	}
	f.deferred = false
	f.data = append(f.data, page.Data...)
	f.next = page.Next
	if len(f.data) > viewerWindow {
		drop := len(f.data) - viewerWindow/2
		f.data = append([]byte(nil), f.data[drop:]...)
		f.base += uint64(drop) //nolint:gosec // the retained window is smaller than the data, so the dropped byte count is positive
	}
}

func (f *viewField) reset() {
	f.base, f.next, f.data, f.err, f.asked = 0, 0, nil, nil, 0
	f.deferred = false
}

func (t *Transcript) layoutReview(gtx layout.Context, env shell.Env, tv *state.ThreadView) {
	r := t.review
	if r == nil {
		return
	}
	if !r.active() {
		r.settleSpill(gtx, env, tv)
		r.bridge.serve(env.View, t.loader.epoch, env.Submit)
		if !env.Modal {
			r.restoreFocus(gtx, t.panes[env.Thread])
		}
		return
	}
	if !r.prepare(gtx, env, tv) {
		return
	}
	if env.Modal {
		gtx = access.Hide(gtx.Disabled())
	} else {
		r.handleKeys(gtx)
		if !r.active() {
			return
		}
		if r.close.Clicked(gtx) {
			r.shut(gtx)
			return
		}
		r.flushCopy(gtx)
	}
	r.bridge.serve(env.View, t.loader.epoch, env.Submit)
	r.step(env.View, t.loader.epoch, env.Submit)
	size := gtx.Constraints.Max
	paint.FillShape(gtx.Ops, t.palette.Surface, clip.Rect{Max: size}.Op())
	defer clip.Rect{Max: size}.Push(gtx.Ops).Pop()
	event.Op(gtx.Ops, &r.focus)
	pointer.CursorDefault.Add(gtx.Ops)
	access.Group(gtx, platform.SemanticNode{ID: "full-text", Role: platform.RoleDialog, Name: "Full text"}, access.Actions{}, func(gtx layout.Context) layout.Dimensions {
		inset := layout.UniformInset(unit.Dp(theme.Space4))
		inset.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return layout.Flex{Axis: layout.Vertical}.Layout(gtx,
				layout.Rigid(func(gtx layout.Context) layout.Dimensions { return r.titleBar(gtx, env) }),
				layout.Rigid(func(gtx layout.Context) layout.Dimensions { return r.findBar(gtx, env) }),
				layout.Flexed(1, func(gtx layout.Context) layout.Dimensions { return r.body(gtx, env) }),
				layout.Rigid(func(gtx layout.Context) layout.Dimensions { return r.footer(gtx, env) }),
			)
		})
		return layout.Dimensions{Size: size}
	})
	r.focusOrder(gtx)
}

func (r *reviewer) prepare(gtx layout.Context, env shell.Env, tv *state.ThreadView) bool {
	if tv == nil || r.thread != env.Thread {
		r.shut(gtx)
		return false
	}
	if r.mount != nil && !r.activateMount(gtx, r.t.panes[env.Thread], tv) {
		return false
	}
	if tv.Link != r.link {
		r.cancelJobs()
		r.link = tv.Link
		for _, f := range r.fields {
			f.reset()
		}
	}
	return true
}

func (r *reviewer) activateMount(gtx layout.Context, p *pane, tv *state.ThreadView) bool {
	m := r.mount
	if p != nil && m.frame == p.frame {
		return false
	}
	if p == nil || p.rows[m.target.id] != m.target || m.target.entry.Item.Header.ItemRevision != m.revision || tv.Link != r.link {
		r.shut(gtx)
		return false
	}
	r.mount = nil
	gtx.Execute(key.FocusCmd{Tag: &r.focus})
	return true
}

func (r *reviewer) titleBar(gtx layout.Context, env shell.Env) layout.Dimensions {
	st := caption(r.t.palette.Text)
	st.size = theme.SizeHeading
	return layout.Flex{Alignment: layout.Middle}.Layout(gtx,
		layout.Flexed(1, func(gtx layout.Context) layout.Dimensions { return label(gtx, env.Shaper, st, "Full text") }),
		layout.Rigid(func(gtx layout.Context) layout.Dimensions { return r.t.ctrl.button(gtx, &r.close, "Close", false) }),
	)
}

func (r *reviewer) findBar(gtx layout.Context, env shell.Env) layout.Dimensions {
	if r.search.Clicked(gtx) && !r.running("find") {
		r.startFind()
	}
	return layout.Inset{Top: unit.Dp(theme.Space2), Bottom: unit.Dp(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		return layout.Flex{Alignment: layout.Middle}.Layout(gtx,
			layout.Flexed(1, func(gtx layout.Context) layout.Dimensions { return r.editor(gtx, env) }),
			layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				return layout.Inset{Left: unit.Dp(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
					return r.t.ctrl.button(gtx, &r.search, "Find", r.running("find"))
				})
			}),
		)
	})
}

func (r *reviewer) editor(gtx layout.Context, env shell.Env) layout.Dimensions {
	p := r.t.palette
	textColor := op.Record(gtx.Ops)
	paint.ColorOp{Color: p.Text}.Add(gtx.Ops)
	textMaterial := textColor.Stop()
	selectColor := op.Record(gtx.Ops)
	sel := p.AccentText
	sel.A = 0x48
	paint.ColorOp{Color: sel}.Add(gtx.Ops)
	selectMaterial := selectColor.Stop()
	macro := op.Record(gtx.Ops)
	dims := layout.UniformInset(unit.Dp(theme.Space2)).Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		semantic.LabelOp("Find in full text").Add(gtx.Ops)
		return r.find.Layout(gtx, platform.EditorStyle{
			Shaper: env.Shaper, Font: giofont.Font{Typeface: platform.ProseFace}, Size: unit.Sp(theme.SizeSmall),
			Text: textMaterial, Selection: selectMaterial,
		})
	})
	call := macro.Stop()
	radius := dp(gtx, theme.RadiusEdit)
	paint.FillShape(gtx.Ops, p.Canvas, clip.UniformRRect(image.Rectangle{Max: dims.Size}, radius).Op(gtx.Ops))
	giodraw.StrokeRRect(gtx, p.Strong, image.Rectangle{Max: dims.Size}, radius, dp(gtx, theme.BorderWidth))
	call.Add(gtx.Ops)
	access.Add(gtx, image.Rectangle{Max: dims.Size}, platform.SemanticNode{
		ID: "query", Role: platform.RoleTextField, Name: "Find in full text", Action: "edit",
	}, access.Actions{Editor: r.find})
	if gtx.Focused(r.find.Tag()) {
		focusRing(gtx, p.Text, dims.Size, radius)
	}
	return dims
}

func (r *reviewer) body(gtx layout.Context, env shell.Env) layout.Dimensions {
	p := r.pane
	p.frame++
	p.pxPerDp = max(gtx.Metric.PxPerDp, 0.01)
	size := gtx.Constraints.Max
	defer clip.Rect{Max: size}.Push(gtx.Ops).Pop()
	event.Op(gtx.Ops, &r.focus)
	r.t.pointerFor(gtx, p, &r.focus)
	rows := make([]*row, 0, len(r.fields))
	for _, f := range r.fields {
		rw := p.row(f.id, synthField)
		rw.field = f
		if f.format == document.FormatPatch && f.complete() && f.base == 0 && len(f.data) != 0 {
			rw.sheet = r.t.reviewDiffSheet(rw, f)
		} else {
			text, err := f.displayText(r.t.loader)
			rw.sheet = sheet{format: f.format, source: text, projectionErr: err}
		}
		if f.format == document.FormatMarkdown && rw.sheet.source == "" {
			rw.sheet.format = document.FormatNotice
		}
		rows = append(rows, rw)
	}
	n := platform.SemanticNode{ID: "review-text", Role: platform.RoleDocument, Name: "Review text", TextContainer: true, TextSelectable: true}
	access.Group(gtx, n, access.Actions{Tag: &r.focus}, func(gtx layout.Context) layout.Dimensions {
		r.t.layoutRows(gtx, env, p, rows)
		return layout.Dimensions{Size: size}
	})
	for _, f := range r.fields {
		f.shown = f.next
	}
	r.revealMatch(p)
	return layout.Dimensions{Size: size}
}

func (r *reviewer) revealMatch(p *pane) {
	r.mu.Lock()
	m := r.match
	r.mu.Unlock()
	if m == nil || m.field >= len(r.fields) {
		return
	}
	f := r.fields[m.field]
	if f.seekMatch(m.offset) {
		return
	}
	e, ok := p.scope.Index(f.id)
	if !ok {
		return
	}
	doc, ok := p.scope.Loaded(e)
	if !ok {
		return
	}
	selection, ok := f.viewSelection(doc, m)
	if !ok {
		return
	}
	if row, ok := p.vp.Locate(p.scope, selection.Anchor); ok {
		p.scrolled = p.vp.RowTop(row) - p.vp.Offset()
	}
	r.mu.Lock()
	if r.match == m {
		r.match = nil
	}
	r.mu.Unlock()
}

func (f *viewField) seekMatch(offset uint64) bool {
	if !f.hasRef || f.convert != nil || f.pretty {
		return false
	}
	if offset < f.base {
		start := offset - offset%uint64(document.PageBytes)
		f.data, f.base, f.next, f.err, f.asked = nil, start, start, nil, 0
		f.pull = true
		return true
	}
	if !f.complete() && offset >= f.base+uint64(len(f.data)) {
		f.pull = true
		return true
	}
	return false
}

func (f *viewField) viewSelection(doc *document.Document, m *matchSpot) (document.Selection, bool) {
	if m.offset < f.base {
		return document.Selection{}, false
	}
	offset := m.offset - f.base
	if offset > uint64(len(doc.Source())) {
		return document.Selection{}, false
	}
	start := int(offset) //nolint:gosec // offset is bounded by the length of a Go string
	if m.length > len(doc.Source())-start {
		return document.Selection{}, false
	}
	from, err1 := doc.Position(start, document.After)
	to, err2 := doc.Position(start+m.length, document.Before)
	return document.Caret(from).Extend(to), err1 == nil && err2 == nil
}

func (r *reviewer) footer(gtx layout.Context, env shell.Env) layout.Dimensions {
	if r.copyAll.Clicked(gtx) && !r.running("copy") {
		r.startCopy()
	}
	if r.export.Clicked(gtx) && !r.running("export") {
		r.startExport()
	}
	r.mu.Lock()
	status := r.status
	r.mu.Unlock()
	return layout.Inset{Top: unit.Dp(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		return layout.Flex{Axis: layout.Vertical}.Layout(gtx,
			layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				if status == "" {
					return layout.Dimensions{}
				}
				return label(gtx, env.Shaper, caption(r.t.palette.Dim), status)
			}),
			layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				return layout.Flex{}.Layout(gtx,
					layout.Rigid(func(gtx layout.Context) layout.Dimensions {
						return r.t.ctrl.button(gtx, &r.copyAll, "Copy all", r.running("copy"))
					}),
					layout.Rigid(func(gtx layout.Context) layout.Dimensions {
						return layout.Inset{Left: unit.Dp(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
							return r.t.ctrl.button(gtx, &r.export, "Export", r.running("export"))
						})
					}),
				)
			}),
		)
	})
}

func (r *reviewer) focusOrder(gtx layout.Context) {
	clear(r.tags)
	r.tags = append(r.tags[:0], &r.close, r.find.Tag(), &r.search, &r.focus)
	for _, placed := range r.pane.placed {
		r.tags = append(r.tags, placed.focusTags...)
	}
	r.tags = append(r.tags, &r.copyAll, &r.export)
	if gtx.Enabled() && !slices.ContainsFunc(r.tags, gtx.Focused) {
		gtx.Execute(key.FocusCmd{Tag: &r.focus})
	}
}

func (r *reviewer) tab(gtx layout.Context, mods key.Modifiers) {
	n := len(r.tags)
	if n == 0 {
		return
	}
	at := slices.IndexFunc(r.tags, gtx.Focused)
	next := (at + 1) % n
	if mods.Contain(key.ModShift) {
		next = n - 1
		if at >= 0 {
			next = (at - 1 + n) % n
		}
	}
	gtx.Execute(key.FocusCmd{Tag: r.tags[next]})
}

func (r *reviewer) updateFind(gtx layout.Context) bool {
	r.find.Clipboard, r.find.Wake = r.t.env.Clipboard, r.t.env.Wake
	for {
		ev, ok := r.find.Update(gtx)
		if !ok {
			return true
		}
		switch ev.Kind {
		case platform.EditorCancel:
			r.shut(gtx)
			return false
		case platform.EditorSubmit:
			if !r.running("find") {
				r.startFind()
			}
		case platform.EditorRejected:
			r.find.TakeRejected()
			r.setStatus(ev.Err.Error())
		case platform.EditorChange, platform.EditorKey:
		}
	}
}

func (r *reviewer) handleKeys(gtx layout.Context) {
	if !r.updateFind(gtx) {
		return
	}
	filters := [...]event.Filter{
		key.FocusFilter{Target: &r.focus},
		key.Filter{Name: key.NameEscape},
		key.Filter{Name: key.NameTab, Optional: key.ModShift},
		key.Filter{Focus: &r.focus, Name: "C", Required: key.ModShortcut},
		key.Filter{Focus: &r.focus, Name: "A", Required: key.ModShortcut},
		key.Filter{Focus: &r.focus, Name: "F", Required: key.ModShortcut},
	}
	for {
		ev, ok := gtx.Event(filters[:]...)
		if !ok {
			return
		}
		e, ok := ev.(key.Event)
		if !ok || e.State != key.Press {
			continue
		}
		if !r.press(gtx, e) {
			return
		}
	}
}

func (r *reviewer) press(gtx layout.Context, e key.Event) bool {
	switch e.Name { //nolint:exhaustive // Gio also accepts arbitrary printable key names.
	case key.NameEscape:
		r.shut(gtx)
		return false
	case key.NameTab:
		r.tab(gtx, e.Modifiers)
	case "C":
		r.copyKeys(gtx)
	case "A":
		r.pane.selectAll()
	case "F":
		gtx.Execute(key.FocusCmd{Tag: r.find.Tag()})
	}
	return true
}

func (r *reviewer) copyKeys(gtx layout.Context) {
	if !r.pane.sel.Selected || !coversAll(r.pane, r.pane.sel.Selection) {
		r.t.copySelection(gtx, r.pane)
		return
	}
	if !r.running("copy") {
		r.startCopy()
	}
}

func (r *reviewer) running(name string) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	if name == "copy" && (r.copied != nil || r.copyErr != nil) {
		return true
	}
	for _, j := range r.jobs {
		if j.name == name {
			return true
		}
	}
	return false
}

func (r *reviewer) commit(gen uint64, apply func()) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	if gen != r.gen {
		return false
	}
	apply()
	return true
}

func (r *reviewer) setStatus(text string) {
	r.mu.Lock()
	r.status = text
	r.mu.Unlock()
}

func (r *reviewer) start(name string, work func(ctx context.Context, gen uint64)) {
	ctx, cancel := context.WithCancel(context.Background())
	r.mu.Lock()
	j := &job{cancel: cancel, name: name, gen: r.gen}
	r.jobs = append(r.jobs, j)
	r.mu.Unlock()
	go func() {
		defer cancel()
		work(ctx, j.gen)
		r.mu.Lock()
		for i, other := range r.jobs {
			if other == j {
				r.jobs = append(r.jobs[:i], r.jobs[i+1:]...)
				break
			}
		}
		r.mu.Unlock()
	}()
}

type logical struct {
	scope   *document.Scope
	fields  map[document.Identity]logicalField
	release []func()
}

type logicalField struct {
	thread     state.ThreadKey
	item       state.ItemKey
	ref        rpc.ContentReference
	live       liveTail
	inline     string
	suffix     string
	bytes      int
	start      int
	recorded   uint64
	projection *selectionProjection
}

type reviewRead uint8

const (
	reviewDisplay reviewRead = iota
	reviewTransfer
)

func (r *reviewer) logical(mode reviewRead) (*logical, error) {
	l := &logical{scope: document.NewScope(string(r.thread.Endpoint), r.thread.Thread), fields: map[document.Identity]logicalField{}}
	refs := make([]document.Ref, 0, len(r.fields))
	for _, f := range r.fields {
		lf, ref, err := f.logicalRef(mode, r.t.loader)
		if err != nil {
			return nil, err
		}
		l.fields[f.id] = lf
		refs = append(refs, ref)
	}
	if err := l.scope.Prepend(refs...); err != nil {
		return nil, err
	}
	if err := l.holdInline(r.t.loader); err != nil {
		l.releaseInline()
		return nil, err
	}
	return l, nil
}

func (l *logical) holdInline(loader *loader) error {
	for _, field := range l.fields {
		text := cmpOr(field.inline, field.suffix)
		if text == "" {
			continue
		}
		drop, ok := loader.holdText(text)
		if !ok {
			return errSourceBudget
		}
		l.release = append(l.release, drop)
	}
	return nil
}

func (l *logical) releaseInline() {
	for _, drop := range l.release {
		drop()
	}
}

func (f *viewField) logicalRef(mode reviewRead, l *loader) (logicalField, document.Ref, error) {
	switch {
	case !f.hasRef:
		return f.inlineSuffixRef()
	case mode == reviewDisplay && (f.convert != nil || f.pretty):
		return f.displayInlineRef(l)
	default:
		return f.recordedSourceRef(mode)
	}
}

func (f *viewField) inlineSuffixRef() (logicalField, document.Ref, error) {
	lf := logicalField{thread: f.thread, item: f.item}
	ref := document.Ref{Header: document.Header{Identity: f.id, Format: document.FormatCode}, Revision: 1}
	lf.inline, ref.Length = f.suffix, len(f.suffix)
	return lf, ref, nil
}

func (f *viewField) displayInlineRef(l *loader) (logicalField, document.Ref, error) {
	lf := logicalField{thread: f.thread, item: f.item}
	ref := document.Ref{Header: document.Header{Identity: f.id, Format: document.FormatCode}, Revision: 1}
	if !f.complete() {
		return lf, ref, errStillLoading
	}
	text, err := f.displayText(l)
	if err != nil {
		return lf, ref, err
	}
	lf.inline = text
	ref.Length = len(lf.inline)
	return lf, ref, nil
}

func (f *viewField) recordedSourceRef(mode reviewRead) (logicalField, document.Ref, error) {
	lf := logicalField{thread: f.thread, item: f.item}
	ref := document.Ref{Header: document.Header{Identity: f.id, Format: document.FormatCode}, Revision: 1}
	if f.total > uint64(math.MaxInt-len(f.suffix)) { //nolint:gosec // len is nonnegative and no greater than MaxInt.
		return lf, ref, errors.New("the full text is too large for this platform")
	}
	lf.ref, lf.live, lf.suffix, lf.recorded = f.ref, f.live, f.suffix, f.recordedBytes()
	lf.bytes = int(f.total) //nolint:gosec // the source and suffix fit in int.
	ref.Length = lf.bytes + len(lf.suffix)
	if mode == reviewTransfer && f.format == document.FormatPatch {
		ref.Header.Format = document.FormatPatch
	}
	return lf, ref, nil
}

type logicalPager struct {
	r *reviewer
	l *logical
}

func (p logicalPager) Page(ctx context.Context, ref document.Ref, offset, limit int) (string, error) {
	lf, ok := p.l.fields[ref.Header.Identity]
	if !ok {
		return "", document.ErrUnknown
	}
	if offset < 0 || offset > ref.Length || limit < 0 {
		return "", textinput.InvalidOffset
	}
	return p.r.readLogical(ctx, lf, offset, min(limit, ref.Length-offset))
}

func (r *reviewer) readLogical(ctx context.Context, lf logicalField, offset, limit int) (string, error) {
	if offset < len(lf.inline) {
		return lf.inline[offset:min(offset+limit, len(lf.inline))], nil
	}
	offset -= len(lf.inline)
	if offset >= lf.bytes {
		at := offset - lf.bytes
		return lf.suffix[at:min(at+limit, len(lf.suffix))], nil
	}
	res := r.bridge.fetch(ctx, lf.request(offset, min(limit, lf.bytes-offset)))
	return res.data, res.err
}

func (f logicalField) request(offset, limit int) pageRequest {
	req := pageRequest{thread: f.thread, item: f.item, ref: f.ref, offset: uint64(f.start + offset), limit: uint64(limit)} //nolint:gosec // Page and capture validate all offsets.
	if f.live.size > 0 && req.offset >= f.recorded {
		req.live, req.offset = f.live, req.offset-f.recorded
		req.limit = min(req.limit, inlineLimit)
	}
	return req
}

func (l *logical) write(ctx context.Context, pager document.Pager, out io.Writer) error {
	if l.scope.Len() == 0 {
		return document.ErrNoPosition
	}
	w := &reviewOutput{out: out, tail: '\n'}
	for i := range l.scope.Len() {
		if err := ctx.Err(); err != nil {
			return err
		}
		if i > 0 {
			if err := w.separate(); err != nil {
				return err
			}
		}
		if err := l.writeField(ctx, i, pager, w); err != nil {
			return err
		}
	}
	return ctx.Err()
}

func (l *logical) writeSelection(ctx context.Context, r *reviewer, pager document.Pager, out io.Writer) error {
	for i := range l.scope.Len() {
		if err := ctx.Err(); err != nil {
			return err
		}
		if i > 0 {
			if _, err := io.WriteString(out, "\n\n"); err != nil {
				return err
			}
		}
		if err := l.writeSelectionRow(ctx, r, pager, out, i); err != nil {
			return err
		}
	}
	return ctx.Err()
}

func (l *logical) writeSelectionRow(ctx context.Context, r *reviewer, pager document.Pager, out io.Writer, i int) error {
	if projection := l.fields[l.scope.Ref(i).Header.Identity].projection; projection != nil {
		return projection.write(ctx, r, out)
	}
	return l.writeField(ctx, i, pager, out)
}

func (l *logical) writeField(ctx context.Context, i int, pager document.Pager, out io.Writer) error {
	ref := l.scope.Ref(i)
	if ref.Header.Format == document.FormatPatch {
		source := reviewSourceReader{size: ref.Length, page: func(offset, limit int) (string, error) {
			return pager.Page(ctx, ref, offset, limit)
		}}
		return streamReviewPatch(ctx, &source, out)
	}
	sel := document.Selection{Anchor: l.scope.Edge(i, false), Focus: l.scope.Edge(i, true)}
	return l.scope.WriteMarkdown(ctx, sel, pager, out)
}

type reviewOutput struct {
	out  io.Writer
	tail byte
}

func (w *reviewOutput) separate() error {
	separator := "\n\n"
	if w.tail == '\n' {
		separator = "\n"
	}
	_, err := w.WriteString(separator)
	return err
}

func (w *reviewOutput) Write(p []byte) (int, error) {
	n, err := w.out.Write(p)
	if n != 0 {
		w.tail = p[n-1]
	}
	return n, err
}

func (w *reviewOutput) WriteString(s string) (int, error) {
	n, err := io.WriteString(w.out, s)
	if n != 0 {
		w.tail = s[n-1]
	}
	return n, err
}

type reviewSourceReader struct {
	size, offset int
	page         func(offset, limit int) (string, error)
	buf          string
}

func (r *reviewSourceReader) Read(p []byte) (int, error) {
	if r.buf == "" {
		if r.offset == r.size {
			return 0, io.EOF
		}
		limit := min(document.PageBytes, r.size-r.offset)
		page, err := r.page(r.offset, limit)
		if err != nil {
			return 0, err
		}
		if len(page) == 0 || len(page) > limit {
			return 0, errNoProgress
		}
		r.buf, r.offset = page, r.offset+len(page)
	}
	n := copy(p, r.buf)
	r.buf = r.buf[n:]
	return n, nil
}

func (r *reviewer) startCopy() {
	l, err := r.logical(reviewTransfer)
	if err != nil {
		r.setStatus("Copy all failed: " + err.Error())
		return
	}
	r.setStatus("Copying the full text")
	r.stageCopy(l, l.write)
}

func (r *reviewer) stageCopy(l *logical, write func(context.Context, document.Pager, io.Writer) error) {
	r.start("copy", func(ctx context.Context, gen uint64) {
		defer l.releaseInline()
		copy, err := stageReviewCopy(ctx, l, logicalPager{r: r, l: l}, write)
		if !r.commit(gen, func() { r.copied, r.copyErr = copy, err }) && copy != nil {
			_ = copy.Close()
		}
	})
}

func (t *Transcript) copySpilledSelection(gtx layout.Context, p *pane) bool {
	span, ok := spillSelection(p)
	if !ok {
		return false
	}
	tv, ok := t.env.View.Thread(p.key)
	if !ok {
		t.notice = "Copy failed: " + errThreadClosed.Error()
		return true
	}
	if t.review == nil {
		t.review = &reviewer{t: t}
	}
	r := t.review
	if r.spill != nil {
		return true
	}
	l, err := span.capture(t, p)
	if err != nil {
		t.notice = "Copy failed: " + err.Error()
		return true
	}
	r.spill, r.link = p, tv.Link
	t.pendingCopy = &pendingCopy{pane: p, all: coversAll(p, p.sel.Selection)}
	t.notice = "Loading the full selection for copy."
	r.stageCopy(l, func(ctx context.Context, pager document.Pager, out io.Writer) error {
		return l.writeSelection(ctx, r, pager, out)
	})
	gtx.Execute(op.InvalidateCmd{})
	return true
}

type spillSpan struct {
	from, to    document.Position
	first, last int
	all         bool
}

func spillSelection(p *pane) (spillSpan, bool) {
	sel := p.sel.Selection
	a, aOK := p.scope.Index(sel.Anchor.Document)
	b, bOK := p.scope.Index(sel.Focus.Document)
	if !aOK || !bOK {
		return spillSpan{}, false
	}
	if a > b || (a == b && sel.Anchor.Offset > sel.Focus.Offset) {
		a, b, sel.Anchor, sel.Focus = b, a, sel.Focus, sel.Anchor
	}
	for i := a; i <= b; i++ {
		if selectedNeedsStaging(p.rows[p.scope.Ref(i).Header.Identity]) {
			return spillSpan{from: sel.Anchor, to: sel.Focus, first: a, last: b, all: coversAll(p, sel)}, true
		}
	}
	return spillSpan{}, false
}

func selectedNeedsStaging(r *row) bool {
	return r != nil && (r.sheet.projectionErr != nil || rowSpill(r) != nil)
}

func rowSpill(r *row) *body {
	if r != nil {
		for _, b := range r.sheet.sources {
			if b.ref.Field == fieldSpill {
				return b
			}
		}
	}
	return nil
}

func (s spillSpan) capture(t *Transcript, p *pane) (*logical, error) {
	l := &logical{
		scope:  document.NewScope(string(p.key.Endpoint), p.key.Thread),
		fields: make(map[document.Identity]logicalField, s.last-s.first+1),
	}
	refs := make([]document.Ref, 0, s.last-s.first+1)
	remaining := document.MaxCopyBytes
	for i := s.first; i <= s.last; i++ {
		selected, err := s.row(t, p, i)
		if err != nil {
			return nil, err
		}
		length := len(selected.inline) + selected.bytes
		if i != s.first {
			remaining -= len("\n\n")
		}
		if length > remaining {
			return nil, platform.ErrTransferTooLarge
		}
		remaining -= length
		selected.inline = strings.Clone(selected.inline)
		id := p.scope.Ref(i).Header.Identity
		l.fields[id] = selected
		refs = append(refs, document.Ref{
			Header:   document.Header{Identity: id, Format: document.FormatCode},
			Revision: 1,
			Length:   length,
		})
	}
	if err := l.scope.Prepend(refs...); err != nil {
		return nil, err
	}
	if err := l.holdInline(t.loader); err != nil {
		l.releaseInline()
		return nil, err
	}
	return l, nil
}

func (s spillSpan) row(t *Transcript, p *pane, i int) (logicalField, error) {
	r := p.rows[p.scope.Ref(i).Header.Identity]
	if r == nil {
		return logicalField{}, errRecordedSourceChanged
	}
	span := s.bounds(p.scope, i)
	if r.sourceEvicted {
		return t.captureOrdinaryRow(p, r, span)
	}
	if r.sheet.projectionErr != nil {
		return logicalField{}, r.sheet.projectionErr
	}
	if b := rowSpill(r); b != nil && r.doc != nil {
		args := p.expanded[r.id] && r.sheet.args
		if !args || !r.sheet.wait {
			return r.spillField(b, span, args)
		}
	}
	return t.captureOrdinaryRow(p, r, span)
}

type spillBounds struct {
	from, to                 document.Position
	partialStart, partialEnd bool
}

func (s spillSpan) bounds(scope *document.Scope, i int) spillBounds {
	b := spillBounds{
		from: scope.Edge(i, false), to: scope.Edge(i, true),
		partialStart: !s.all && i == s.first, partialEnd: !s.all && i == s.last,
	}
	if b.partialStart {
		b.from = s.from
	}
	if b.partialEnd {
		b.to = s.to
	}
	return b
}

func (r *row) spillField(b *body, span spillBounds, args bool) (logicalField, error) {
	if b.total > uint64(math.MaxInt) {
		return logicalField{}, platform.ErrTransferTooLarge
	}
	f := logicalField{thread: b.key.thread, item: b.key.item, ref: b.ref, bytes: int(b.total)} //nolint:gosec // validated above.
	if span.partialStart || span.partialEnd || args {
		window, err := r.spillWindow(b, args)
		if err != nil {
			return logicalField{}, err
		}
		if err := window.clip(&f, r.doc.Source(), span); err != nil {
			return logicalField{}, err
		}
	}
	if f.bytes > document.MaxCopyBytes-len(f.inline) {
		return logicalField{}, platform.ErrTransferTooLarge
	}
	return f, nil
}

type spillWindow struct {
	raw                   []byte
	base, leading, prefix int
	kind                  Kind
}

func (w spillWindow) clip(f *logicalField, shown string, span spillBounds) error {
	from, to := span.from.Offset, span.to.Offset
	if from < 0 || to < from || to > len(shown) {
		return textinput.InvalidOffset
	}
	start, end := 0, f.bytes
	if span.partialStart && from >= w.prefix {
		start = w.offset(from)
	}
	if span.partialEnd {
		end = start
		if to > w.prefix {
			end = max(start, w.offset(to))
		}
	}
	f.start, f.bytes = start, end-start
	f.inline = shown[min(from, w.prefix):min(to, w.prefix)]
	return nil
}

func (r *row) spillWindow(b *body, args bool) (spillWindow, error) {
	if !b.complete() {
		return spillWindow{}, errStillLoading
	}
	w := spillWindow{raw: b.data, base: int(b.start), kind: r.entry.Kind} //nolint:gosec // spillField validates total, which bounds start.
	if w.base > 0 {
		cut := bytes.IndexByte(w.raw, '\n') + 1
		w.base += cut
		w.raw = w.raw[cut:]
	}
	shown := r.doc.Source()
	if r.pres.Mode == ModeTail && !b.full {
		w.leading = w.tailLength() - len(shown)
	} else if args {
		w.prefix = len(shown) - w.projectedLength()
	}
	if w.leading < 0 || w.prefix < 0 || !w.matches(shown[w.prefix:]) {
		return spillWindow{}, errMismatch
	}
	return w, nil
}

func (w spillWindow) tailLength() int {
	n := w.projectedLength()
	if n > 0 && w.raw[w.rawOffset(n-1)] == '\n' {
		return n - 1
	}
	return n
}

func (w spillWindow) lines(yield func(int, []byte) bool) {
	n := 0
	for line := range bytes.Lines(w.raw) {
		if (w.kind != KindTool || !anchorLine.Match(bytes.TrimSuffix(line, []byte("\n")))) && !yield(n, line) {
			return
		}
		n += len(line)
	}
}

func (w spillWindow) projectedLength() int {
	size, end := 0, 0
	for at, line := range w.lines {
		size += len(line)
		end = at + len(line)
	}
	if end > 0 && end < len(w.raw) && w.raw[len(w.raw)-1] != '\n' {
		size--
	}
	return size
}

func (w spillWindow) matches(shown string) bool {
	skip := w.leading
	for _, line := range w.lines {
		if skip >= len(line) {
			skip -= len(line)
			continue
		}
		line = line[skip:min(len(line), skip+len(shown))]
		if !bytes.HasPrefix([]byte(shown), line) {
			return false
		}
		shown = shown[len(line):]
		if shown == "" {
			return true
		}
		skip = 0
	}
	return shown == ""
}

func (w spillWindow) offset(at int) int {
	return w.base + w.rawOffset(w.leading+at-w.prefix)
}

func (w spillWindow) rawOffset(at int) int {
	end := 0
	for raw, line := range w.lines {
		if at < len(line) {
			return raw + at
		}
		at -= len(line)
		end = raw + len(line)
	}
	return end
}

func (r *reviewer) settleSpill(gtx layout.Context, env shell.Env, tv *state.ThreadView) {
	if r.spill == nil {
		return
	}
	if tv == nil || r.spill.key != env.Thread || tv.Link != r.link {
		r.cancelJobs()
		r.spill, r.t.pendingCopy = nil, nil
		r.t.notice = "Copy cancelled because the thread changed."
		return
	}
	if !env.Modal {
		r.flushCopy(gtx)
	}
}

type reviewCopy struct {
	file *os.File
	size int
	tail [utf8.UTFMax]byte
	used int
}

func stageReviewCopy(ctx context.Context, l *logical, pager document.Pager, write func(context.Context, document.Pager, io.Writer) error) (*reviewCopy, error) {
	if err := l.checkCopyExtent(); err != nil {
		return nil, err
	}
	file, err := os.CreateTemp("", "dalgona-clipboard-*")
	if err != nil {
		return nil, fmt.Errorf("stage clipboard data: %w", err)
	}
	c := &reviewCopy{file: file}
	err = write(ctx, pager, c)
	if err == nil && c.used != 0 {
		err = textinput.InvalidEncoding
	}
	if err == nil {
		err = ctx.Err()
	}
	if err == nil {
		_, err = file.Seek(0, io.SeekStart)
	}
	if err != nil {
		return nil, errors.Join(err, c.Close())
	}
	return c, nil
}

func (l *logical) checkCopyExtent() error {
	if l.scope.Len() == 0 {
		return document.ErrNoPosition
	}
	size := 0
	for i := range l.scope.Len() {
		ref := l.scope.Ref(i)
		if ref.Header.Format == document.FormatPatch {
			continue
		}
		if ref.Length > document.MaxCopyBytes-size {
			return platform.ErrTransferTooLarge
		}
		size += ref.Length
	}
	return nil
}

func (c *reviewCopy) Write(data []byte) (int, error) {
	return c.WriteString(string(data))
}

func (c *reviewCopy) WriteString(text string) (int, error) {
	if len(text) > document.MaxCopyBytes-c.size {
		return 0, platform.ErrTransferTooLarge
	}
	if strings.IndexByte(text, 0) >= 0 {
		return 0, textinput.DisallowedControl
	}
	if err := c.checkEncoding(text); err != nil {
		return 0, err
	}
	n, err := c.file.WriteString(text)
	c.size += n
	return n, err
}

func (c *reviewCopy) checkEncoding(text string) error {
	text, err := c.consumeTail(text)
	if err != nil {
		return err
	}
	if utf8.ValidString(text) {
		return nil
	}
	for start := len(text) - 1; start >= 0 && start > len(text)-utf8.UTFMax; start-- {
		if utf8.RuneStart(text[start]) && !utf8.FullRuneInString(text[start:]) && utf8.ValidString(text[:start]) {
			c.used = copy(c.tail[:], text[start:])
			return nil
		}
	}
	return textinput.InvalidEncoding
}

func (c *reviewCopy) consumeTail(text string) (string, error) {
	if c.used == 0 {
		return text, nil
	}
	for len(text) > 0 && !utf8.FullRune(c.tail[:c.used]) {
		c.tail[c.used] = text[0]
		c.used++
		text = text[1:]
	}
	if !utf8.FullRune(c.tail[:c.used]) {
		return "", nil
	}
	if _, width := utf8.DecodeRune(c.tail[:c.used]); width == 1 {
		return "", textinput.InvalidEncoding
	}
	c.used = 0
	return text, nil
}

func (c *reviewCopy) Read(data []byte) (int, error) { return c.file.Read(data) }

func (c *reviewCopy) Close() error {
	return errors.Join(c.file.Close(), os.Remove(c.file.Name()))
}

func (r *reviewer) flushCopy(gtx layout.Context) {
	if !gtx.Enabled() {
		return
	}
	r.mu.Lock()
	copy, err := r.copied, r.copyErr
	r.copied, r.copyErr = nil, nil
	r.mu.Unlock()
	if copy == nil && err == nil {
		return
	}
	if copy != nil {
		gtx.Execute(clipboard.WriteCmd{Type: "application/text", Data: copy})
	}
	if r.spill != nil {
		r.spill, r.t.pendingCopy = nil, nil
		r.t.notice = "Copied the selection."
		if err != nil {
			r.t.notice = "Copy failed: " + err.Error()
		}
		return
	}
	if err != nil {
		r.setStatus("Copy all failed: " + err.Error())
		return
	}
	r.setStatus("Copied the full text: " + byteSize(uint64(copy.size))) //nolint:gosec // size is a count of bytes successfully written.
}

func (r *reviewer) startExport() {
	save := r.t.deps.Save
	if save == nil {
		r.setStatus("Export failed: " + errNoSave.Error())
		return
	}
	l, err := r.logical(reviewTransfer)
	if err != nil {
		r.setStatus("Export failed: " + err.Error())
		return
	}
	r.setStatus("Choose where to save the full text")
	r.start("export", func(ctx context.Context, gen uint64) {
		defer l.releaseInline()
		choice, err := save(ctx, platform.DialogRequest{Kind: platform.DialogSaveFile, Title: "Export full text", SuggestedName: "full-text.txt"})
		if err != nil || choice.Cancelled || len(choice.Files) != 1 {
			r.commit(gen, func() { r.status = exportOutcome(err, choice) })
			return
		}
		err = document.SaveFile(ctx, choice.Files[0], func(w io.Writer) error {
			return l.write(ctx, logicalPager{r: r, l: l}, w)
		})
		if err != nil {
			r.commit(gen, func() { r.status = "Export failed; the destination was not replaced: " + err.Error() })
			return
		}
		r.commit(gen, func() { r.status = "Exported the full text to " + choice.Files[0] })
	})
}

func exportOutcome(err error, choice platform.DialogResult) string {
	switch {
	case err != nil:
		return "Export failed: " + err.Error()
	case choice.Cancelled:
		return "Export cancelled."
	}
	return "Export failed: choose exactly one destination file."
}

func (r *reviewer) startFind() {
	query := r.find.Draft().Text
	if query == "" {
		r.setStatus("Type text to find.")
		return
	}
	l, err := r.logical(reviewDisplay)
	if err != nil {
		r.setStatus("Find failed: " + err.Error())
		return
	}
	fields := len(r.fields)
	r.start("find", func(ctx context.Context, gen uint64) {
		defer l.releaseInline()
		count, first, err := l.find(ctx, logicalPager{r: r, l: l}, query)
		if err != nil {
			r.commit(gen, func() { r.status = "Find stopped: " + err.Error() })
			return
		}
		scope := "in " + countLabel(fields, "section", "sections") + " of the full text"
		status := countLabel(count, "match", "matches") + " " + scope
		r.commit(gen, func() {
			r.status = status
			if first != nil {
				r.match = first
			}
		})
	})
}

func (l *logical) find(ctx context.Context, pager document.Pager, query string) (int, *matchSpot, error) {
	count := 0
	var first *matchSpot
	for i := range l.scope.Len() {
		match, err := findField(ctx, pager, l.scope.Ref(i), query)
		if err != nil {
			return 0, nil, err
		}
		if first == nil && match.count > 0 {
			first = &matchSpot{field: i, offset: uint64(match.first), length: len(query)} //nolint:gosec // the match offset counts bytes from the start of the field
		}
		count += match.count
	}
	return count, first, nil
}

type fieldMatch struct {
	count int
	first int
}

func findField(ctx context.Context, pager document.Pager, ref document.Ref, query string) (fieldMatch, error) {
	var match fieldMatch
	pending := ""
	for offset := 0; offset < ref.Length; {
		page, err := pager.Page(ctx, ref, offset, document.PageBytes)
		if err != nil {
			return fieldMatch{}, err
		}
		if page == "" {
			return fieldMatch{}, errNoProgress
		}
		window := pending + page
		base := offset - len(pending)
		final := offset+len(page) >= ref.Length
		bound := len(window)
		if !final {
			bound = max(len(window)-(len(query)-1), 0)
		}
		pending = match.scanWindow(window, query, base, bound)
		offset += len(page)
		if ctx.Err() != nil {
			return fieldMatch{}, ctx.Err()
		}
	}
	return match, nil
}

func (m *fieldMatch) scanWindow(window, query string, base, bound int) string {
	pos := 0
	for pos < bound {
		at := strings.Index(window[pos:], query)
		if at < 0 || pos+at >= bound {
			break
		}
		if m.count == 0 {
			m.first = base + pos + at
		}
		m.count++
		pos += at + len(query)
	}
	return window[min(max(pos, bound), len(window)):]
}
