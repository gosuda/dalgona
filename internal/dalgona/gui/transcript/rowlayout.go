//go:build gui

package transcript

import (
	"image"
	"image/color"
	"sort"
	"strconv"
	"strings"

	"gioui.org/f32"
	giofont "gioui.org/font"
	"gioui.org/io/event"
	"gioui.org/io/key"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const infinite = 1 << 24

type frame struct {
	gtx       layout.Context
	env       shell.Env
	pane      *pane
	row       *row
	ref       document.RowRef
	focusTags []event.Tag
	column    int
	y         int
}

func (f *frame) stack(w layout.Widget) layout.Dimensions {
	gtx := f.gtx
	gtx.Constraints = layout.Constraints{Max: image.Pt(f.column, infinite)}
	stack := op.Offset(image.Pt(0, f.y)).Push(gtx.Ops)
	dims := w(gtx)
	stack.Pop()
	f.y += dims.Size.Y
	return dims
}

func rowFrame(gtx layout.Context, env shell.Env, p *pane, i int) (*frame, int, int) {
	ref := p.vp.Row(i)
	r := p.order[ref.Entry]
	size := gtx.Constraints.Max
	pad := dp(gtx, theme.Space6)
	column := max(min(size.X-2*pad, dp(gtx, theme.TranscriptMaxWidth)), dp(gtx, theme.Space8))
	indent := 0
	if r.entry.Nested {
		indent = dp(gtx, theme.Space5)
	}
	inner := dp(gtx, theme.Space3)
	x0 := (size.X-column)/2 + indent
	return &frame{gtx: gtx, env: env, pane: p, row: r, ref: ref, column: max(column-indent-2*inner, 1)}, x0, column
}

func (f *frame) addFocus(tag event.Tag) {
	if f.row.field != nil && f.gtx.Enabled() {
		f.focusTags = append(f.focusTags, tag)
	}
}

func (t *Transcript) layoutRow(gtx layout.Context, env shell.Env, p *pane, i int) placed {
	f, x0, column := rowFrame(gtx, env, p, i)
	ref, r := f.ref, f.row
	inner := dp(gtx, theme.Space3)
	first := ref.Block <= 0 && ref.Row == 0
	last := i+1 >= p.vp.Rows() || p.vp.Row(i+1).Entry != ref.Entry
	info := placed{index: i, ref: ref, row: r, block: ref.Block, last: last}
	gap := 0
	if first && ref.Entry > 0 {
		gap = dp(gtx, theme.Space5)
	}
	macro := op.Record(gtx.Ops)
	shift := op.Offset(image.Pt(x0+inner, gap)).Push(gtx.Ops)
	t.rowContents(f, &info, image.Pt(x0+inner, gap), first)
	shift.Pop()
	content := macro.Stop()
	height := gap + f.y
	t.card(gtx, r, image.Rect(x0, gap, x0+f.column+2*inner, height))
	if i == p.vp.Rows()-1 {
		t.lastFocus(gtx, p, image.Rect(x0, 0, x0+column, height))
	}
	content.Add(gtx.Ops)
	info.focusTags = f.focusTags
	info.height = height
	return info
}

func (t *Transcript) rowContents(f *frame, info *placed, at image.Point, first bool) {
	if first {
		f.stack(func(gtx layout.Context) layout.Dimensions { return t.header(gtx, f.env, f.pane, f.row) })
	}
	if f.ref.Block >= 0 && f.ref.Block < f.row.doc.Len() {
		info.layout, info.split, info.text = t.text(f)
		info.text = info.text.Add(at)
	}
	if info.last {
		f.stack(func(gtx layout.Context) layout.Dimensions { return t.footer(gtx, f) })
	}
}

func (t *Transcript) lastFocus(gtx layout.Context, p *pane, area image.Rectangle) {
	for {
		if _, ok := gtx.Event(key.FocusFilter{Target: &p.lastTag}); !ok {
			break
		}
	}
	defer clip.Rect(area).Push(gtx.Ops).Pop()
	event.Op(gtx.Ops, &p.lastTag)
	if gtx.Focused(&p.lastTag) {
		focusRing(gtx, t.palette.Text, area.Size(), 0)
	}
}

func (t *Transcript) card(gtx layout.Context, r *row, area image.Rectangle) {
	if r.synth != synthNone {
		return
	}
	switch r.entry.Kind {
	case KindUser:
		giodraw.FillRect(gtx, t.palette.Tone, area)
	case KindAdvisor:
		edge := area
		edge.Max.X = edge.Min.X + dp(gtx, theme.SelectionBarWidth)
		giodraw.FillRect(gtx, advisorEdge(t.palette, r.sheet.severity), edge)
	case KindCommand, KindTool, KindFileChange, KindGroup:
		edge := area
		edge.Max.X = edge.Min.X + dp(gtx, theme.BorderWidth)
		giodraw.FillRect(gtx, t.palette.Line, edge)
	case KindAssistant, KindThinking, KindPlanItem, KindCompaction, KindOther:
	}
}

func advisorEdge(p theme.Palette, severity string) color.NRGBA {
	switch severity {
	case "concern":
		return p.AccentText
	case "blocker":
		return p.FailedText
	}
	return p.Strong
}

func (t *Transcript) text(f *frame) (*document.RowLayout, *splitLayout, image.Point) {
	gtx, r, ref := f.gtx, f.row, f.ref
	b := r.doc.Block(ref.Block)
	if ref.Row == 0 && ref.Block > 0 {
		f.y += dp(gtx, theme.Space2)
	}
	if dims, ok := t.special(f, b); ok {
		f.y += dims.Size.Y
		return nil, nil, image.Point{}
	}
	line := r.lineOf(b, ref.Row)
	if r.sheet.source == "" && r.doc.Len() == 1 {
		return nil, nil, image.Point{}
	}
	diff := r.diffLine(line)
	t.textActions(f, b, diff, line)
	if diff.pair {
		s, at := t.paintSplit(f, b, diff)
		return nil, s, at
	}
	rl, at := t.paintText(f, b, diff, line)
	return rl, nil, at
}

func (t *Transcript) textActions(f *frame, b document.Block, diff diffLine, line int) {
	switch {
	case b.Kind == document.KindCode && f.ref.Row == 0:
		t.codeAction(f, b)
	case diff.role == roleFile || diff.role == roleHunk:
		if diff.start == b.Span.Start+b.Rows[f.ref.Row].Start {
			t.diffToolbar(f, line)
		}
	}
}

func (t *Transcript) paintText(f *frame, b document.Block, diff diffLine, line int) (*document.RowLayout, image.Point) {
	gtx, r, ref := f.gtx, f.row, f.ref
	role := r.role(line)
	at := image.Pt(0, f.y)
	width := f.column - t.extrasWidth(f, line) - gutterWidth(gtx, diff)
	at.X = gutterWidth(gtx, diff)
	t.diffGutter(gtx, f.env, image.Pt(0, f.y), diff.old, 0)
	t.diffGutter(gtx, f.env, image.Pt(gutterCell(gtx), f.y), diff.new, 1)
	cgtx := textContext(gtx, width)
	rl := t.shape(cgtx, f, b, role)
	stack := op.Offset(at).Push(gtx.Ops)
	area := clip.Rect{Max: rl.Size()}.Push(gtx.Ops)
	span := b.Rows[ref.Row]
	semantic.LabelOp(strings.TrimRight(b.Projection.Text[span.Start:span.End], "\n")).Add(gtx.Ops)
	t.rowSemantics(gtx, r, line, role)
	area.Pop()
	ink := roleColor(t.palette, role)
	rl.SetTokens(t.codeTokens(r, b))
	rl.Paint(cgtx, f.env.Shaper, documentPalette(t.palette, ink), f.pane.selectedSpan(r, ref.Block, b.Rows[ref.Row]))
	t.paintFormulas(cgtx, b, rl, ink)
	t.underline(gtx, r, b, ref.Row, rl)
	stack.Pop()
	t.extras(f, line, rl.Size().Y, width+at.X)
	f.y += rl.Size().Y
	t.foldNote(f)
	return rl, at
}

func textContext(gtx layout.Context, width int) layout.Context {
	gtx.Constraints = layout.Constraints{Max: image.Pt(max(width, 1), infinite)}
	return gtx
}

func (t *Transcript) navigationLayout(gtx layout.Context, env shell.Env, p *pane, i int, measured map[int]laid) (*document.Flow, int, bool) {
	f, _, _ := rowFrame(gtx, env, p, i)
	if f.ref.Block < 0 || f.ref.Block >= f.row.doc.Len() {
		return nil, 0, false
	}
	if l, ok := measured[i]; ok {
		if flow := l.info.flow(); flow != nil {
			return flow, l.info.text.Y, true
		}
	}
	b := f.row.doc.Block(f.ref.Block)
	line := f.row.lineOf(b, f.ref.Row)
	d := f.row.diffLine(line)
	if d.pair {
		return t.splitGeometry(f, b, d).flow(), 0, true
	}
	width := f.column - t.extrasWidth(f, line) - gutterWidth(gtx, d)
	rl := t.shape(textContext(gtx, width), f, b, f.row.role(line))
	for _, pl := range p.placed {
		if pl.index == i && pl.layout == rl {
			return &rl.Flow, pl.text.Y, true
		}
	}
	return &rl.Flow, 0, true
}

func (t *Transcript) foldNote(f *frame) {
	if f.ref.Hidden <= 0 {
		return
	}
	f.stack(func(gtx layout.Context) layout.Dimensions {
		return label(gtx, f.env.Shaper, caption(t.palette.Dim), "… "+countLabel(f.ref.Hidden, "more line", "more lines")+" folded")
	})
}

func (t *Transcript) shape(gtx layout.Context, f *frame, b document.Block, role lineRole) *document.RowLayout {
	ref := f.ref
	key := document.NewGeometryKey(f.row.doc, ref.Block, ref.Row)
	key.Width, key.Scale, key.Roles = gtx.Constraints.Max.X, gtx.Metric.PxPerDp, uint32(role)
	typo := typography(1)
	if t.mathReady() {
		typo.Inline = t.inlineBox(gtx)
		key.Fonts = formulaBoxes(typo.Inline, b, b.Rows[ref.Row])
	}
	if rl, ok := t.geometry.Get(key); ok {
		return rl
	}
	if role == roleFile {
		typo.Mono.Weight = giofont.Bold
	}
	if role == roleFailed {
		typo.Body.Weight = giofont.Bold
	}
	rl := document.ShapeRow(gtx, f.env.Shaper, b, ref.Row, typo)
	t.geometry.Put(key, rl, rl.Bytes())
	return rl
}

func (t *Transcript) rowSemantics(gtx layout.Context, r *row, line int, role lineRole) {
	if r.synth == synthPlan && role == roleCurrent {
		semantic.SelectedOp(true).Add(gtx.Ops)
		semantic.DescriptionOp("current step").Add(gtx.Ops)
	}
	if line < len(r.sheet.roles) && (role == roleAdd || role == roleDelete) {
		semantic.DescriptionOp(map[lineRole]string{roleAdd: "added line", roleDelete: "removed line"}[role]).Add(gtx.Ops)
	}
}

func (r *row) lineOf(b document.Block, rowIndex int) int {
	start := b.Span.Start + b.Rows[rowIndex].Start
	src := r.doc.Source()
	if len(r.sheet.diffLines) > 0 {
		return sort.Search(len(r.sheet.diffLines), func(i int) bool { return r.sheet.diffLines[i].start > start }) - 1
	}
	if r.doc.Format() == document.FormatMarkdown {
		return 0
	}
	return strings.Count(src[:min(start, len(src))], "\n")
}

func (r *row) role(line int) lineRole {
	if line < len(r.sheet.roles) {
		return r.sheet.roles[line]
	}
	return roleText
}

func (p *pane) selectedSpan(r *row, block int, rowSpan document.Span) document.Span {
	if !p.sel.Selected || p.sel.Selection.Collapsed() {
		return document.Span{}
	}
	e, ok := p.scope.Index(r.id)
	if !ok {
		return document.Span{}
	}
	a, okA := p.resolve(p.sel.Selection.Anchor)
	z, okZ := p.resolve(p.sel.Selection.Focus)
	if !okA || !okZ {
		return document.Span{}
	}
	if less(z, a) {
		a, z = z, a
	}
	return clipSelection(spot{entry: e, block: block}, rowSpan, a, z)
}

func clipSelection(here spot, rowSpan document.Span, a, z spot) document.Span {
	first, last := spot{entry: a.entry, block: a.block}, spot{entry: z.entry, block: z.block}
	start, end := rowSpan.Start, rowSpan.End
	switch {
	case less(here, first):
		return document.Span{}
	case here == first:
		start = max(start, a.text)
	}
	switch {
	case less(last, here):
		return document.Span{}
	case here == last:
		end = min(end, z.text)
	}
	if start >= end {
		return document.Span{}
	}
	return document.Span{Start: start, End: end}
}

type spot struct {
	entry, block, text int
}

func less(a, b spot) bool {
	if a.entry != b.entry {
		return a.entry < b.entry
	}
	if a.block != b.block {
		return a.block < b.block
	}
	return a.text < b.text
}

func (p *pane) resolve(pos document.Position) (spot, bool) {
	e, ok := p.scope.Index(pos.Document)
	if !ok {
		return spot{}, false
	}
	doc, ok := p.scope.Loaded(e)
	if !ok {
		return spot{}, false
	}
	key, t, err := doc.TextOffset(pos, textinput.Bytes)
	if err != nil {
		return spot{}, false
	}
	b := sort.Search(doc.Len(), func(i int) bool { return doc.Block(i).Span.End > pos.Offset })
	for i := max(b-1, 0); i < min(b+2, doc.Len()); i++ {
		if doc.Block(i).Key == key {
			return spot{entry: e, block: i, text: t}, true
		}
	}
	return spot{}, false
}

func (t *Transcript) underline(gtx layout.Context, r *row, b document.Block, rowIndex int, rl *document.RowLayout) {
	rowSpan := b.Rows[rowIndex]
	thick := max(dp(gtx, theme.BorderWidth), 1)
	for _, k := range r.sheet.keywords {
		span := document.Span{Start: max(k.Start-b.Span.Start, rowSpan.Start), End: min(k.End-b.Span.Start, rowSpan.End)}
		if span.Start >= span.End {
			continue
		}
		for _, rect := range rl.Highlight(span) {
			giodraw.FillRect(gtx, t.palette.AccentText, image.Rect(int(rect.X0), int(rect.Y1)-thick, int(rect.X1+0.5), int(rect.Y1)))
		}
	}
}

type rowAction struct {
	click *widget.Clickable
	name  string
	act   func(f *frame)
}

func (r *row) action(ref document.RowRef, line int, scope *document.Scope) (rowAction, bool) {
	if r.synth == synthChildren && line < len(r.children) {
		link := r.children[line]
		return rowAction{click: &link.open, name: "Open", act: func(f *frame) { f.env.Navigate(link.key) }}, true
	}
	return r.foldAction(ref, scope)
}

func (t *Transcript) extrasWidth(f *frame, line int) int {
	if _, ok := f.row.action(f.ref, line, f.pane.scope); ok {
		return dp(f.gtx, theme.QuestionActionWidth)
	}
	return 0
}

func (t *Transcript) extras(f *frame, line, height, width int) {
	a, ok := f.row.action(f.ref, line, f.pane.scope)
	if !ok {
		return
	}
	if a.click.Clicked(f.gtx) {
		a.act(f)
	}
	gtx := f.gtx
	gtx.Constraints = layout.Constraints{Max: image.Pt(f.column-width, max(height, dp(gtx, theme.TargetMin)))}
	stack := op.Offset(image.Pt(width, f.y)).Push(gtx.Ops)
	t.ctrl.button(gtx, a.click, a.name, false)
	stack.Pop()
}

func (t *Transcript) meta(gtx layout.Context, env shell.Env, words ...string) layout.FlexChild {
	text := strings.Join(nonEmpty(words), " · ")
	return layout.Rigid(func(gtx layout.Context) layout.Dimensions {
		if text == "" {
			return layout.Dimensions{}
		}
		return layout.Inset{Left: unit.Dp(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return label(gtx, env.Shaper, caption(t.palette.Dim), text)
		})
	})
}

func nonEmpty(words []string) []string {
	out := words[:0:0]
	for _, w := range words {
		if w != "" {
			out = append(out, w)
		}
	}
	return out
}

func (t *Transcript) tag(env shell.Env, status string) layout.FlexChild {
	return layout.Rigid(func(gtx layout.Context) layout.Dimensions {
		if status == "" {
			return layout.Dimensions{}
		}
		return layout.Inset{Left: unit.Dp(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return shell.ItemTag(status).Layout(gtx, env)
		})
	})
}

func (t *Transcript) header(gtx layout.Context, env shell.Env, p *pane, r *row) layout.Dimensions {
	title := headerTitle(r)
	e := r.entry
	var children []layout.FlexChild
	switch {
	case r.synth == synthNone && e.Kind == KindThinking && r.pres.Mode == ModeFull:
		children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			return t.toggleButton(gtx, p, r, "Thinking")
		}))
	case r.synth == synthNone && e.Kind == KindCommand:
		command := reader{loader: t.loader, thread: e.Thread, item: e.Item}.text("command")
		children = append(children, layout.Flexed(1, func(gtx layout.Context) layout.Dimensions {
			return label(gtx, env.Shaper, mono(t.palette.Text), "$ "+command.text)
		}))
	case title != "":
		children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			return label(gtx, env.Shaper, caption(t.palette.Dim), title)
		}))
	}
	children = t.headerExtras(gtx, env, p, r, children)
	if len(children) == 0 {
		return layout.Dimensions{}
	}
	return layout.Inset{Bottom: unit.Dp(theme.Space1)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		return layout.Flex{Alignment: layout.Middle}.Layout(gtx, children...)
	})
}

func (t *Transcript) headerExtras(gtx layout.Context, env shell.Env, p *pane, r *row, children []layout.FlexChild) []layout.FlexChild {
	if r.synth != synthNone {
		return children
	}
	e := r.entry
	if toolKind(e.Kind) {
		children = append(children, t.tag(env, t.displayStatus(e)), t.meta(gtx, env, exitWord(e), durationWord(e)))
	}
	if e.Kind == KindTool && r.sheet.args {
		children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			return layout.Inset{Left: unit.Dp(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
				return t.toggleButton(gtx, p, r, "Arguments")
			})
		}))
	}
	return children
}

func headerTitle(r *row) string {
	switch r.synth {
	case synthPlan:
		return "Plan"
	case synthChildren:
		return "Subagents"
	case synthField:
		return strings.Join(nonEmpty([]string{r.field.title, r.field.meta}), " · ")
	case synthNone:
		if r.entry.Kind == KindThinking && r.pres.Mode == ModeTail {
			return "Thinking"
		}
		return entryHeaderTitle(r)
	case synthHistory, synthOutcome, synthReview:
		return ""
	}
	return ""
}

func entryHeaderTitle(r *row) string {
	switch r.entry.Kind {
	case KindUser:
		return "You"
	case KindAssistant:
		return "dalgona"
	case KindThinking, KindCommand, KindCompaction, KindOther, KindGroup:
	case KindTool:
		return cmpOr(r.entry.Facts.tool, kindLabel(r.entry.Item.Header.Type))
	case KindFileChange:
		return "File changes"
	case KindAdvisor:
		return advisorLabel(r.sheet.advisor, r.sheet.severity)
	case KindPlanItem:
		return "Plan"
	}
	return ""
}

func exitWord(e Entry) string {
	if e.Kind != KindCommand || e.Facts.exit == nil {
		return ""
	}
	return "exit " + strconv.FormatInt(*e.Facts.exit, 10)
}

func durationWord(e Entry) string {
	if e.Facts.duration == nil {
		return ""
	}
	return formatDuration(*e.Facts.duration)
}

func (t *Transcript) displayStatus(e Entry) string {
	switch {
	case t.awaiting(e):
		return statusAwaiting
	case t.expired(e):
		return statusTimedOut
	}
	return e.status()
}

func (t *Transcript) toggleButton(gtx layout.Context, p *pane, r *row, name string) layout.Dimensions {
	expanded, set := p.expanded[r.id]
	if !set {
		expanded = r.entry.Kind == KindThinking && r.entry.Live()
	}
	if r.toggle.Clicked(gtx) {
		expanded = !expanded
		p.expanded[r.id] = expanded
	}
	return r.toggle.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		semantic.Button.Add(gtx.Ops)
		semantic.LabelOp(name).Add(gtx.Ops)
		semantic.SelectedOp(expanded).Add(gtx.Ops)
		mark := "▸ "
		if expanded {
			mark = "▾ "
		}
		dims := paintLabel(gtx, t.env.Shaper, caption(t.palette.Text), mark+name)
		if gtx.Focused(&r.toggle) {
			focusRing(gtx, t.palette.Text, dims.Size, dp(gtx, theme.RadiusEdit))
		}
		return dims
	})
}

func (r *row) diffLine(i int) diffLine {
	if i < 0 || i >= len(r.sheet.diffLines) {
		return diffLine{}
	}
	return r.sheet.diffLines[i]
}

func gutterCell(gtx layout.Context) int {
	return dp(gtx, theme.Space8+theme.Space4)
}

func gutterWidth(gtx layout.Context, d diffLine) int {
	if d.old == 0 && d.new == 0 {
		return 0
	}
	return 2 * gutterCell(gtx)
}

func (t *Transcript) diffGutter(gtx layout.Context, env shell.Env, at image.Point, number, side int) {
	if number == 0 {
		return
	}
	defer op.Offset(at).Push(gtx.Ops).Pop()
	gtx = textContext(gtx, gutterCell(gtx))
	word := strconv.Itoa(number)
	dims := paintLabel(gtx, env.Shaper, mono(t.palette.Dim), word)
	defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
	prefix := "Old line "
	if side == 1 {
		prefix = "New line "
	}
	semantic.LabelOp(prefix + word).Add(gtx.Ops)
}

type splitCell struct {
	layout            *document.RowLayout
	at, width, scroll int
	role              lineRole
}

type splitLayout struct {
	cells         [2]splitCell
	span          document.Span
	width, height int
}

func (s *splitLayout) hit(at f32.Point) (int, document.Affinity) {
	i := 0
	if int(at.X) >= s.width/2 {
		i = 1
	}
	c := s.cells[i]
	if c.layout == nil {
		c = s.cells[1-i]
	}
	if c.layout == nil {
		return s.span.Start, document.After
	}
	x := min(max(int(at.X)-c.at, 0), c.width)
	return c.layout.Hit(f32.Pt(float32(x+c.scroll), at.Y))
}

func (s *splitLayout) flow() *document.Flow {
	f := &document.Flow{Row: s.span, Height: float32(s.height), Width: float32(s.width)}
	for i, c := range s.cells {
		if c.layout == nil || i == 1 && s.cells[0].layout != nil && c.layout.Row == s.cells[0].layout.Row {
			continue
		}
		l := c.layout
		f.Text = l.Text
		base := len(f.Clusters)
		f.Clusters = append(f.Clusters, l.Clusters...)
		for _, x := range l.X {
			f.X = append(f.X, x+float32(c.at-c.scroll))
		}
		for _, j := range l.Order {
			f.Order = append(f.Order, j+base)
		}
	}
	f.Lines = []document.Line{{Text: s.span, Clusters: document.Span{End: len(f.Order)}, Bottom: f.Height, Width: f.Width}}
	return f
}

func (s *splitLayout) semanticLines(span document.Span) []platform.TextLine {
	var out []platform.TextLine
	for i, c := range s.cells {
		if c.layout == nil || i == 1 && s.cells[0].layout != nil && c.layout.Row == s.cells[0].layout.Row {
			continue
		}
		for _, line := range c.layout.SemanticLines(span) {
			line.Bounds = line.Bounds.Add(image.Pt(c.at-c.scroll, 0))
			left, right := c.at, c.at+c.width
			line.Bounds.Min.X = min(max(line.Bounds.Min.X, left), right)
			line.Bounds.Max.X = min(max(line.Bounds.Max.X, left), right)
			for j := range line.Clusters {
				cluster := &line.Clusters[j]
				cluster.Left = min(max(cluster.Left+c.at-c.scroll, left), right)
				cluster.Right = min(max(cluster.Right+c.at-c.scroll, left), right)
			}
			out = append(out, line)
		}
	}
	return out
}

func (t *Transcript) splitGeometry(f *frame, b document.Block, d diffLine) *splitLayout {
	ref, r := f.ref, f.row
	span := b.Rows[ref.Row]
	s := &splitLayout{span: span, width: f.column}
	endOld := d.start + d.cut - b.Span.Start
	for i, number := range [...]int{d.old, d.new} {
		half := f.column / 2
		gutter := min(gutterCell(f.gtx), half/2)
		c := &s.cells[i]
		c.at, c.width = i*half+gutter, max(half-gutter, 1)
		sub := span
		switch {
		case d.context:
			c.role = roleText
		case i == 0:
			c.role, sub.End = roleDelete, min(sub.End, endOld)
		case i == 1:
			c.role = roleAdd
			if d.cut > 0 {
				sub.Start = max(sub.Start, endOld+1)
			}
		}
		if number == 0 || sub.Len() <= 0 {
			continue
		}
		key := document.NewGeometryKey(r.doc, ref.Block, ref.Row)
		key.Width, key.Scale, key.Roles = infinite, f.gtx.Metric.PxPerDp, uint32(1<<20+i) //nolint:gosec // i is a cell index (0 or 1).
		rl, ok := t.geometry.Get(key)
		if !ok {
			part := b
			part.Rows = []document.Span{sub}
			rl = document.ShapeRow(textContext(f.gtx, infinite), f.env.Shaper, part, 0, typography(1))
			t.geometry.Put(key, rl, rl.Bytes())
		}
		c.layout = rl
		r.diffActions.maxWidth[i] = max(r.diffActions.maxWidth[i], rl.Width)
		c.scroll = int(min(r.diffActions.horizontal[i], max(r.diffActions.maxWidth[i]-float32(c.width), 0)))
		s.height = max(s.height, rl.Size().Y)
	}
	return s
}

func (t *Transcript) paintSplit(f *frame, b document.Block, d diffLine) (*splitLayout, image.Point) {
	s := t.splitGeometry(f, b, d)
	gtx := f.gtx
	at := image.Pt(0, f.y)
	selected := f.pane.selectedSpan(f.row, f.ref.Block, b.Rows[f.ref.Row])
	giodraw.FillRect(gtx, t.palette.Line, image.Rect(s.width/2, f.y, s.width/2+dp(gtx, theme.BorderWidth), f.y+s.height))
	for i, c := range s.cells {
		if c.layout == nil {
			continue
		}
		t.diffGutter(gtx, f.env, image.Pt(i*s.width/2, f.y), [...]int{d.old, d.new}[i], i)
		t.paintSplitCell(gtx, f.env, c, f.y, selected)
	}
	f.y += s.height
	return s, at
}

func (t *Transcript) paintSplitCell(gtx layout.Context, env shell.Env, c splitCell, y int, selected document.Span) {
	defer op.Offset(image.Pt(c.at, y)).Push(gtx.Ops).Pop()
	defer clip.Rect{Max: image.Pt(c.width, c.layout.Size().Y)}.Push(gtx.Ops).Pop()
	semantic.LabelOp(strings.TrimRight(c.layout.Text[c.layout.Row.Start:c.layout.Row.End], "\n")).Add(gtx.Ops)
	defer op.Offset(image.Pt(-c.scroll, 0)).Push(gtx.Ops).Pop()
	c.layout.Paint(gtx, env.Shaper, documentPalette(t.palette, roleColor(t.palette, c.role)), selected)
}
