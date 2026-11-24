package document

import (
	"slices"
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

type Cluster struct {
	Text    Span
	Advance float32
	Ascent  float32
	Descent float32
	RTL     bool
	Break   bool
	Object  bool
	Style   Style
	Glyphs  Span
}

type Line struct {
	Clusters Span
	Text     Span
	Top      float32
	Baseline float32
	Bottom   float32
	Width    float32
}

type Flow struct {
	Text     string
	Row      Span
	Clusters []Cluster
	Order    []int
	X        []float32
	Lines    []Line
	Width    float32
	Height   float32
	Inset    float32
}

type Measure struct {
	MaxWidth   float32
	LineHeight float32
	Tab        func(x float32) float32
	RTL        bool
	Inset      float32
}

func Arrange(text string, row Span, clusters []Cluster, m Measure) Flow {
	f := Flow{Text: text, Row: row, Clusters: clusters, X: make([]float32, len(clusters)), Order: make([]int, 0, len(clusters)), Inset: m.Inset}
	start := 0
	for start < len(clusters) || len(f.Lines) == 0 {
		end := f.lineEnd(start, m)
		f.place(start, end, m)
		if end == start {
			break
		}
		start = end
	}
	return f
}

func (f *Flow) lineEnd(start int, m Measure) int {
	x, opportunity := float32(0), -1
	for i := start; i < len(f.Clusters); i++ {
		if f.newline(i) {
			return i + 1
		}
		width := f.advance(i, x, m)
		if f.overflows(i, start, x+width, m) {
			return breakPoint(opportunity, start, i)
		}
		x += width
		if f.Clusters[i].Break {
			opportunity = i
		}
	}
	return len(f.Clusters)
}

func breakPoint(opportunity, start, at int) int {
	if opportunity >= start {
		return opportunity + 1
	}
	return at
}

func (f *Flow) advance(i int, x float32, m Measure) float32 {
	if f.tab(i) && m.Tab != nil {
		return m.Tab(x) - x
	}
	return f.Clusters[i].Advance
}

func (f *Flow) overflows(i, start int, right float32, m Measure) bool {
	return right > m.MaxWidth && i > start && (!f.Clusters[i].Break || !f.space(i))
}

func (f *Flow) place(start, end int, m Measure) {
	line := Line{Clusters: Span{Start: len(f.Order), End: len(f.Order)}, Text: f.textSpan(start, end)}
	ascent, descent := float32(0), float32(0)
	x := float32(0)
	for _, i := range visualOrder(f.Clusters, start, end, m.RTL) {
		c := f.Clusters[i]
		f.X[i] = x
		f.Clusters[i].Advance = f.advance(i, x, m)
		x += f.Clusters[i].Advance
		ascent, descent = max(ascent, c.Ascent), max(descent, c.Descent)
		f.Order = append(f.Order, i)
	}
	line.Clusters.End = len(f.Order)
	height := max(m.LineHeight, ascent+descent)
	line.Top = f.Height
	line.Baseline = f.Height + (height-ascent-descent)/2 + ascent
	line.Bottom = f.Height + height
	line.Width = x
	f.Height = line.Bottom
	f.Width = max(f.Width, x)
	f.Lines = append(f.Lines, line)
}

func (f *Flow) textSpan(start, end int) Span {
	if start == end {
		at := f.Row.End
		if start < len(f.Clusters) {
			at = f.Clusters[start].Text.Start
		} else if len(f.Clusters) == 0 {
			at = f.Row.Start
		}
		return Span{Start: at, End: at}
	}
	return Span{Start: f.Clusters[start].Text.Start, End: f.Clusters[end-1].Text.End}
}

func visualOrder(clusters []Cluster, start, end int, rtl bool) []int {
	order := make([]int, 0, end-start)
	for i := start; i < end; {
		j := i + 1
		for j < end && clusters[j].RTL == clusters[i].RTL {
			j++
		}
		order = appendRun(order, i, j, clusters[i].RTL != rtl)
		i = j
	}
	if rtl {
		slices.Reverse(order)
	}
	return order
}

func appendRun(order []int, from, to int, reversed bool) []int {
	for k := range to - from {
		if reversed {
			order = append(order, to-1-k)
		} else {
			order = append(order, from+k)
		}
	}
	return order
}

func (f *Flow) first(i int) rune {
	r, _ := utf8.DecodeRuneInString(f.Text[f.Clusters[i].Text.Start:f.Clusters[i].Text.End])
	return r
}

func (f *Flow) newline(i int) bool {
	c := f.Clusters[i].Text
	return c.Len() > 0 && f.Text[c.End-1] == '\n'
}

func (f *Flow) tab(i int) bool {
	return f.first(i) == '\t'
}

func (f *Flow) space(i int) bool {
	r := f.first(i)
	return r == ' ' || r == '\u3000' || r == '\t'
}

func (f *Flow) lineAt(y float32) int {
	for i, line := range f.Lines {
		if y < line.Bottom {
			return i
		}
	}
	return len(f.Lines) - 1
}

func (f *Flow) lineOf(offset int, affinity Affinity) int {
	for i, line := range f.Lines {
		if offset < line.Text.End || offset == line.Text.End && f.endsLine(i, affinity) {
			return i
		}
	}
	return len(f.Lines) - 1
}

func (f *Flow) endsLine(i int, affinity Affinity) bool {
	text := f.Lines[i].Text
	if i == len(f.Lines)-1 {
		return true
	}
	return affinity == Before && !strings.HasSuffix(f.Text[text.Start:text.End], "\n")
}

func (f *Flow) Hit(x, y float32) (int, Affinity) {
	line := f.Lines[f.lineAt(y)]
	if line.Clusters.Len() == 0 {
		return line.Text.Start, After
	}
	for _, i := range f.Order[line.Clusters.Start:line.Clusters.End] {
		c := f.Clusters[i]
		if x >= f.X[i]+c.Advance || f.newline(i) {
			continue
		}
		return f.hitCluster(i, x-f.X[i])
	}
	if x < 0 {
		return line.Text.Start, After
	}
	return f.contentEnd(line), Before
}

func (f *Flow) contentEnd(line Line) int {
	text := f.Text[line.Text.Start:line.Text.End]
	switch {
	case strings.HasSuffix(text, "\r\n"):
		return line.Text.End - 2
	case strings.HasSuffix(text, "\n"):
		return line.Text.End - 1
	}
	return line.Text.End
}

func (f *Flow) hitCluster(i int, x float32) (int, Affinity) {
	c := f.Clusters[i]
	if c.Advance <= 0 {
		return c.Text.Start, After
	}
	edges := clusterEdges(f.Text, c)
	slot := min(int(max(x, 0)/c.Advance*float32(len(edges)-1)), len(edges)-2)
	part := c.Advance / float32(len(edges)-1)
	near := x-float32(slot)*part < part/2
	if c.RTL {
		near = !near
		slot = len(edges) - 2 - slot
	}
	if near {
		return edges[slot], After
	}
	return edges[slot+1], Before
}

func clusterEdges(text string, c Cluster) []int {
	if c.Object {
		return []int{c.Text.Start, c.Text.End}
	}
	inner := inputtext.Boundaries(text[c.Text.Start:c.Text.End])
	edges := make([]int, 0, max(len(inner), 2))
	for _, b := range inner {
		edges = append(edges, c.Text.Start+b)
	}
	if len(edges) < 2 {
		edges = []int{c.Text.Start, c.Text.End}
	}
	return edges
}

func (f *Flow) Caret(offset int, affinity Affinity) (float32, float32, float32) {
	line := f.Lines[f.lineOf(offset, affinity)]
	x, ok := f.caretIn(line, offset, affinity)
	if !ok {
		x, _ = f.caretIn(line, offset, affinity.opposite())
	}
	return x, line.Top, line.Bottom
}

func (f *Flow) caretIn(line Line, offset int, affinity Affinity) (float32, bool) {
	for _, i := range f.Order[line.Clusters.Start:line.Clusters.End] {
		c := f.Clusters[i].Text
		if affinity == After && c.Start <= offset && offset < c.End || affinity == Before && c.Start < offset && offset <= c.End {
			return f.X[i] + f.within(i, offset), true
		}
	}
	return 0, false
}

func (f *Flow) within(i, offset int) float32 {
	c := f.Clusters[i]
	edges := clusterEdges(f.Text, c)
	part := c.Advance / float32(len(edges)-1)
	for k, edge := range edges {
		if edge >= offset {
			if c.RTL {
				return c.Advance - float32(k)*part
			}
			return float32(k) * part
		}
	}
	return c.Advance
}

type Rect struct {
	X0, Y0, X1, Y1 float32
}

func (f *Flow) Highlight(selected Span) []Rect {
	var rects []Rect
	for _, line := range f.Lines {
		for _, i := range f.Order[line.Clusters.Start:line.Clusters.End] {
			c := f.Clusters[i]
			if c.Text.End <= selected.Start || c.Text.Start >= selected.End || f.newline(i) {
				continue
			}
			x0 := f.X[i] + f.within(i, max(selected.Start, c.Text.Start))
			x1 := f.X[i] + f.within(i, min(selected.End, c.Text.End))
			rects = appendRect(rects, Rect{X0: min(x0, x1), Y0: line.Top, X1: max(x0, x1), Y1: line.Bottom})
		}
	}
	return rects
}

func appendRect(rects []Rect, r Rect) []Rect {
	if n := len(rects); n > 0 && rects[n-1].Y0 == r.Y0 && rects[n-1].X1 == r.X0 {
		rects[n-1].X1 = r.X1
		return rects
	}
	return append(rects, r)
}

func (f *Flow) LineTop(offset int, affinity Affinity) float32 {
	_, top, _ := f.Caret(offset, affinity)
	return top
}

func (a Affinity) opposite() Affinity {
	if a == Before {
		return After
	}
	return Before
}

type Placement struct {
	Text     Span
	Rect     Rect
	Baseline float32
}

func (f *Flow) Objects() []Placement {
	var placed []Placement
	for _, line := range f.Lines {
		for _, i := range f.Order[line.Clusters.Start:line.Clusters.End] {
			if c := f.Clusters[i]; c.Object {
				x := f.Inset + f.X[i]
				placed = append(placed, Placement{Text: c.Text, Rect: Rect{X0: x, Y0: line.Baseline - c.Ascent, X1: x + c.Advance, Y1: line.Baseline + c.Descent}, Baseline: line.Baseline})
			}
		}
	}
	return placed
}
