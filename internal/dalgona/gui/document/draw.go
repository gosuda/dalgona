//go:build gui

package document

import (
	"image"
	"image/color"
	"slices"
	"strconv"
	"unicode/utf8"

	"gioui.org/f32"
	"gioui.org/font"
	"gioui.org/io/system"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/op/paint"
	"gioui.org/text"
	"gioui.org/unit"

	"github.com/gosuda/dalgona/internal/dalgona/gui/document/code"
)

type InlineBox struct {
	Width   float32
	Ascent  float32
	Descent float32
}

type Palette struct {
	Text           color.NRGBA
	Muted          color.NRGBA
	Link           color.NRGBA
	Code           color.NRGBA
	CodeBackground color.NRGBA
	Selection      color.NRGBA
	Rule           color.NRGBA
	Token          func(code.Kind) color.NRGBA
}

type Typography struct {
	Body       font.Font
	Heading    font.Font
	Mono       font.Font
	Size       unit.Sp
	CodeSize   unit.Sp
	Headings   [6]unit.Sp
	LineHeight float32
	TabSpaces  int
	Indent     unit.Dp
	Inline     func(b Block, object Run) (InlineBox, bool)
	Tokens     []code.Span
}

type RowLayout struct {
	Flow
	block  Block
	row    int
	glyphs []text.Glyph
	width  float32
	marker []text.Glyph
	tokens []code.Span
}

func (r *RowLayout) SetTokens(spans []code.Span) {
	r.tokens = spans
}

func (r *RowLayout) tokenKind(at int) code.Kind {
	lo, hi := 0, len(r.tokens)
	for lo < hi {
		mid := (lo + hi) / 2
		if r.tokens[mid].End <= at {
			lo = mid + 1
		} else {
			hi = mid
		}
	}
	if lo < len(r.tokens) && r.tokens[lo].Start <= at {
		return r.tokens[lo].Kind
	}
	return code.PlainText
}

type segment struct {
	text  Span
	style Style
}

func ShapeRow(gtx layout.Context, shaper *text.Shaper, b Block, row int, t Typography) *RowLayout {
	r := &RowLayout{block: b, row: row, width: float32(gtx.Constraints.Max.X), tokens: t.Tokens}
	inset := float32(len(b.Containers) * gtx.Dp(t.Indent))
	span := b.Rows[row]
	var clusters []Cluster
	for _, s := range segments(b.Projection, span) {
		clusters = r.shapeSegment(gtx, shaper, t, s, clusters)
	}
	measure := Measure{
		MaxWidth:   max(r.width-inset, 1),
		LineHeight: float32(gtx.Sp(t.sizeFor(b, 0))) * t.LineHeight,
		Tab:        r.tabStops(gtx, shaper, t, max(r.width-inset, 1)),
		RTL:        gtx.Locale.Direction.Progression() == system.TowardOrigin,
		Inset:      inset,
	}
	r.Flow = Arrange(b.Projection.Text, span, clusters, measure)
	if measure.RTL || r.wrapped() || r.bidirectional() {
		r.probe(gtx, shaper, t, span)
		r.Flow = Arrange(b.Projection.Text, span, r.Clusters, measure)
	}
	r.marker = r.shapeMarker(gtx, shaper, t)
	return r
}

func segments(p Projection, span Span) []segment {
	var out []segment
	for _, run := range p.Runs {
		start, end := max(run.Text.Start, span.Start), min(run.Text.End, span.End)
		if run.Style&InlineMath != 0 && start < end {
			out = append(out, segment{text: Span{Start: start, End: end}, style: run.Style})
			continue
		}
		for at := start; at < end; {
			next := at + controlLength(p.Text, at, end)
			if next == at {
				next = plainEnd(p.Text, at, end)
			}
			out = appendSegment(out, segment{text: Span{Start: at, End: next}, style: run.Style}, p.Text)
			at = next
		}
	}
	return out
}

func plainEnd(text string, at, end int) int {
	for at < end && controlLength(text, at, end) == 0 {
		at++
	}
	return at
}

func appendSegment(out []segment, s segment, text string) []segment {
	if n := len(out); n > 0 && out[n-1].style == s.style && s.style&InlineMath == 0 && out[n-1].text.End == s.text.Start &&
		!control(text, out[n-1].text) && !control(text, s.text) {
		out[n-1].text.End = s.text.End
		return out
	}
	return append(out, s)
}

func controlLength(text string, at, end int) int {
	switch {
	case text[at] == '\n' || text[at] == '\t':
		return 1
	case text[at] == '\r' && at+1 < end && text[at+1] == '\n':
		return 2
	}
	return 0
}

func control(text string, s Span) bool {
	return s.Len() > 0 && controlLength(text, s.Start, s.End) == s.Len()
}

func (r *RowLayout) emptyCluster(s segment) Cluster {
	return Cluster{Text: s.text, Style: s.style, Glyphs: Span{Start: len(r.glyphs), End: len(r.glyphs)}}
}

func (r *RowLayout) shapeSegment(gtx layout.Context, shaper *text.Shaper, t Typography, s segment, clusters []Cluster) []Cluster {
	source := r.block.Projection.Text
	if control(source, s.text) {
		return append(clusters, r.emptyCluster(s))
	}
	if box, ok := r.inlineBox(t, s); ok {
		c := r.emptyCluster(s)
		c.Advance, c.Ascent, c.Descent, c.Object = box.Width, box.Ascent, box.Descent, true
		return append(clusters, c)
	}
	shaper.LayoutString(text.Parameters{
		Font:     t.fontFor(r.block, s.style),
		PxPerEm:  em(text.Parameters{}.PxPerEm, gtx.Sp(t.sizeFor(r.block, s.style))),
		MaxWidth: 1 << 24,
		Locale:   gtx.Locale,
	}, source[s.text.Start:s.text.End])
	at := s.text.Start
	current := r.emptyCluster(segment{text: Span{Start: at, End: at}, style: s.style})
	for g, ok := shaper.NextGlyph(); ok; g, ok = shaper.NextGlyph() {
		r.glyphs = append(r.glyphs, g)
		current.Glyphs.End = len(r.glyphs)
		current.Advance += float32(g.Advance) / 64
		current.Ascent = max(current.Ascent, float32(g.Ascent)/64)
		current.Descent = max(current.Descent, float32(g.Descent)/64)
		current.RTL = g.Flags&text.FlagTowardOrigin != 0
		if g.Flags&text.FlagClusterBreak == 0 {
			continue
		}
		at = advanceRunes(source, at, s.text.End, int(g.Runes))
		current.Text.End = at
		clusters = append(clusters, current)
		current = r.emptyCluster(segment{text: Span{Start: at, End: at}, style: s.style})
	}
	if n := len(clusters); n > 0 && clusters[n-1].Text.End < s.text.End {
		clusters[n-1].Text.End = s.text.End
	}
	return clusters
}

func (r *RowLayout) inlineBox(t Typography, s segment) (InlineBox, bool) {
	if s.style&InlineMath == 0 || t.Inline == nil {
		return InlineBox{}, false
	}
	p := r.block.Projection
	return t.Inline(r.block, p.Runs[p.runAt(s.text.Start)])
}

func (r *RowLayout) wrapped() bool {
	for _, line := range r.Lines[:len(r.Lines)-1] {
		if r.Text[line.Text.End-1] != '\n' {
			return true
		}
	}
	return false
}

func (r *RowLayout) bidirectional() bool {
	return slices.ContainsFunc(r.Clusters, func(c Cluster) bool { return c.RTL })
}

func (r *RowLayout) probe(gtx layout.Context, shaper *text.Shaper, t Typography, span Span) {
	source := r.block.Projection.Text
	shaper.LayoutString(text.Parameters{
		Font:       t.fontFor(r.block, 0),
		PxPerEm:    em(text.Parameters{}.PxPerEm, gtx.Sp(t.sizeFor(r.block, 0))),
		MaxWidth:   1,
		WrapPolicy: text.WrapWords,
		Locale:     gtx.Locale,
	}, source[span.Start:span.End])
	at, next := span.Start, 0
	for g, ok := shaper.NextGlyph(); ok; g, ok = shaper.NextGlyph() {
		if g.Flags&text.FlagClusterBreak == 0 {
			continue
		}
		end := advanceRunes(source, at, span.End, int(g.Runes))
		next = r.mark(next, end, g.Flags)
		at = end
	}
}

func (r *RowLayout) mark(next, end int, flags text.Flags) int {
	for ; next < len(r.Clusters) && r.Clusters[next].Text.Start < end; next++ {
		r.Clusters[next].RTL = flags&text.FlagTowardOrigin != 0
	}
	if flags&text.FlagLineBreak != 0 && next > 0 && r.Clusters[next-1].Text.End == end {
		r.Clusters[next-1].Break = true
	}
	return next
}

func em[T ~int32](_ T, px int) T {
	return T(px * 64)
}

func moved[T ~int32](v T, px float32) T {
	return v + T(px*64)
}

func advanceRunes(text string, at, limit, runes int) int {
	for ; runes > 0 && at < limit; runes-- {
		_, width := utf8.DecodeRuneInString(text[at:limit])
		at += width
	}
	return at
}

func (t Typography) fontFor(b Block, style Style) font.Font {
	f := t.Body
	switch {
	case style&(InlineCode|RawMarkup|InlineMath) != 0 || monospaced(b.Kind):
		f = t.Mono
	case b.Kind == KindHeading:
		f = t.Heading
	}
	if style&Strong != 0 {
		f.Weight = font.Bold
	}
	if style&Emphasis != 0 {
		f.Style = font.Italic
	}
	return f
}

func monospaced(kind Kind) bool {
	switch kind {
	case KindCode, KindDiff, KindHTML, KindFigure, KindMath:
		return true
	case KindParagraph, KindHeading, KindTable, KindRule, KindNotice:
		return false
	}
	return false
}

func (t Typography) sizeFor(b Block, style Style) unit.Sp {
	switch {
	case style&InlineCode != 0 || monospaced(b.Kind):
		return t.CodeSize
	case b.Kind == KindHeading && b.Level >= 1 && b.Level <= len(t.Headings):
		return t.Headings[b.Level-1]
	default:
		return t.Size
	}
}

func (r *RowLayout) tabStops(gtx layout.Context, shaper *text.Shaper, t Typography, width float32) func(float32) float32 {
	if r.block.Kind == KindTable {
		column := width / float32(max(len(r.block.Alignments), 1))
		return func(x float32) float32 { return (float32(int(x/column)) + 1) * column }
	}
	shaper.LayoutString(text.Parameters{Font: t.Mono, PxPerEm: em(text.Parameters{}.PxPerEm, gtx.Sp(t.CodeSize)), MaxWidth: 1 << 24}, " ")
	space := float32(0)
	for g, ok := shaper.NextGlyph(); ok; g, ok = shaper.NextGlyph() {
		space += float32(g.Advance) / 64
	}
	stop := max(space*float32(max(t.TabSpaces, 1)), 1)
	return func(x float32) float32 { return (float32(int(x/stop)) + 1) * stop }
}

func (r *RowLayout) shapeMarker(gtx layout.Context, shaper *text.Shaper, t Typography) []text.Glyph {
	if len(r.block.Containers) == 0 || r.row != 0 {
		return nil
	}
	item := r.block.Containers[len(r.block.Containers)-1]
	if item.Kind != ContainerListItem || !item.First {
		return nil
	}
	if r.block.Task != TaskNone {
		return nil
	}
	label := "•"
	if item.Ordered {
		label = strconv.Itoa(item.Number) + string(rune(item.Marker))
	}
	shaper.LayoutString(text.Parameters{Font: t.Body, PxPerEm: em(text.Parameters{}.PxPerEm, gtx.Sp(t.Size)), MaxWidth: 1 << 24, Locale: gtx.Locale}, label)
	var glyphs []text.Glyph
	for g, ok := shaper.NextGlyph(); ok; g, ok = shaper.NextGlyph() {
		glyphs = append(glyphs, g)
	}
	return glyphs
}

func (r *RowLayout) Size() image.Point {
	return image.Pt(int(r.width), int(r.Height+0.5))
}

func (r *RowLayout) Bytes() int {
	return len(r.glyphs)*64 + len(r.Clusters)*72 + len(r.Lines)*48 + len(r.Order)*12
}

func (r *RowLayout) Hit(p f32.Point) (int, Affinity) {
	return r.Flow.Hit(p.X-r.Inset, p.Y)
}

func (r *RowLayout) Caret(offset int, affinity Affinity) (f32.Point, float32) {
	x, top, bottom := r.Flow.Caret(offset, affinity)
	return f32.Pt(x+r.Inset, top), bottom - top
}

func (r *RowLayout) Paint(gtx layout.Context, shaper *text.Shaper, p Palette, selected Span) layout.Dimensions {
	r.paintChrome(gtx, shaper, p)
	shift := op.Offset(image.Pt(int(r.Inset), 0)).Push(gtx.Ops)
	for _, rect := range r.Highlight(selected) {
		fill(gtx, p.Selection, rect)
	}
	for _, line := range r.Lines {
		r.paintLine(gtx, shaper, p, line)
	}
	shift.Pop()
	return layout.Dimensions{Size: r.Size()}
}

func fill(gtx layout.Context, c color.NRGBA, rect Rect) {
	bounds := image.Rect(int(rect.X0), int(rect.Y0), int(rect.X1+0.5), int(rect.Y1+0.5))
	paint.FillShape(gtx.Ops, c, clip.Rect(bounds).Op())
}

func (r *RowLayout) paintChrome(gtx layout.Context, shaper *text.Shaper, p Palette) {
	indent := float32(0)
	if len(r.block.Containers) > 0 {
		indent = r.Inset / float32(len(r.block.Containers))
	}
	for level, c := range r.block.Containers {
		if c.Kind == ContainerQuotation {
			x := float32(level) * indent
			fill(gtx, p.Rule, Rect{X0: x, Y0: 0, X1: x + 2, Y1: r.Height})
		}
	}
	if r.block.Kind == KindRule {
		middle := r.Height / 2
		fill(gtx, p.Rule, Rect{X0: r.Inset, Y0: middle, X1: r.width, Y1: middle + 1})
	}
	if len(r.marker) > 0 && len(r.Lines) > 0 {
		r.paintGlyphs(gtx, shaper, p.Muted, r.marker, f32.Pt(r.Inset-indent, r.Lines[0].Baseline))
	}
	r.paintTask(gtx, p)
}

func (r *RowLayout) paintTask(gtx layout.Context, p Palette) {
	if r.block.Task == TaskNone || r.row != 0 || len(r.block.Containers) == 0 || len(r.Lines) == 0 {
		return
	}
	item := r.block.Containers[len(r.block.Containers)-1]
	if item.Kind != ContainerListItem || !item.First {
		return
	}
	line := r.Lines[0]
	side := min(max(line.Baseline-line.Top, 8), 16)
	x := r.Inset - float32(len(r.block.Containers))*side - 4
	if x < 0 {
		x = 0
	}
	y := line.Baseline - side
	th := max(1, side/8)
	fill(gtx, p.Muted, Rect{X0: x, Y0: y, X1: x + side, Y1: y + th})
	fill(gtx, p.Muted, Rect{X0: x, Y0: y + side - th, X1: x + side, Y1: y + side})
	fill(gtx, p.Muted, Rect{X0: x, Y0: y, X1: x + th, Y1: y + side})
	fill(gtx, p.Muted, Rect{X0: x + side - th, Y0: y, X1: x + side, Y1: y + side})
	if r.block.Task == TaskDone {
		fill(gtx, p.Text, Rect{X0: x + 2*th, Y0: y + 2*th, X1: x + side - 2*th, Y1: y + side - 2*th})
	}
}

func (r *RowLayout) paintLine(gtx layout.Context, shaper *text.Shaper, p Palette, line Line) {
	order := r.Order[line.Clusters.Start:line.Clusters.End]
	for start := 0; start < len(order); {
		style := r.Clusters[order[start]].Style
		end := start + 1
		for end < len(order) && r.Clusters[order[end]].Style == style {
			end++
		}
		r.paintPiece(gtx, shaper, p, line, order[start:end])
		start = end
	}
}

func (r *RowLayout) paintPiece(gtx layout.Context, shaper *text.Shaper, p Palette, line Line, clusters []int) {
	if len(clusters) == 0 {
		return
	}
	first, last := clusters[0], clusters[len(clusters)-1]
	left, right, style := r.X[first], r.X[last]+r.Clusters[last].Advance, r.Clusters[first].Style
	if style&InlineCode != 0 {
		fill(gtx, p.CodeBackground, Rect{X0: left, Y0: line.Top, X1: right, Y1: line.Bottom})
	}
	r.paintTokens(gtx, shaper, p, line, clusters)
	if style&LinkText != 0 {
		fill(gtx, p.Link, Rect{X0: left, Y0: line.Baseline + 1, X1: right, Y1: line.Baseline + 2})
	}
	if style&Strikethrough != 0 {
		middle := line.Baseline - (line.Baseline-line.Top)/3
		fill(gtx, styleColor(p, style), Rect{X0: left, Y0: middle, X1: right, Y1: middle + 1})
	}
}

func (r *RowLayout) paintTokens(gtx layout.Context, shaper *text.Shaper, p Palette, line Line, clusters []int) {
	base := styleColor(p, r.Clusters[clusters[0]].Style)
	for start := 0; start < len(clusters); {
		kind := r.tokenKind(r.Clusters[clusters[start]].Text.Start)
		end := start + 1
		for end < len(clusters) && r.tokenKind(r.Clusters[clusters[end]].Text.Start) == kind {
			end++
		}
		var glyphs []text.Glyph
		for _, i := range clusters[start:end] {
			glyphs = r.placeCluster(glyphs, i)
		}
		if len(glyphs) > 0 {
			r.paintGlyphs(gtx, shaper, p.tokenColor(kind, base), glyphs, f32.Pt(float32(glyphs[0].X)/64, line.Baseline))
		}
		start = end
	}
}

func (r *RowLayout) placeCluster(glyphs []text.Glyph, i int) []text.Glyph {
	c := r.Clusters[i]
	if c.Glyphs.Len() == 0 {
		return glyphs
	}
	origin := r.glyphs[c.Glyphs.Start].X
	for _, g := range r.glyphs[c.Glyphs.Start:c.Glyphs.End] {
		origin = min(origin, g.X)
	}
	for _, g := range r.glyphs[c.Glyphs.Start:c.Glyphs.End] {
		g.X = moved(g.X-origin, r.X[i])
		glyphs = append(glyphs, g)
	}
	return glyphs
}

func (p Palette) tokenColor(kind code.Kind, base color.NRGBA) color.NRGBA {
	if kind == code.PlainText || p.Token == nil {
		return base
	}
	return p.Token(kind)
}

func (r *RowLayout) paintGlyphs(gtx layout.Context, shaper *text.Shaper, c color.NRGBA, glyphs []text.Glyph, origin f32.Point) {
	transform := op.Affine(f32.AffineId().Offset(origin)).Push(gtx.Ops)
	outline := clip.Outline{Path: shaper.Shape(glyphs)}.Op().Push(gtx.Ops)
	paint.ColorOp{Color: c}.Add(gtx.Ops)
	paint.PaintOp{}.Add(gtx.Ops)
	outline.Pop()
	if call := shaper.Bitmaps(glyphs); call != (op.CallOp{}) {
		call.Add(gtx.Ops)
	}
	transform.Pop()
}

func styleColor(p Palette, style Style) color.NRGBA {
	switch {
	case style&LinkText != 0:
		return p.Link
	case style&(InlineCode|RawMarkup|InlineMath) != 0:
		return p.Code
	case style&ImageText != 0:
		return p.Muted
	default:
		return p.Text
	}
}
