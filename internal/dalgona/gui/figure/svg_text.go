package figure

import (
	"strings"
)

var refusedTextAttributes = []string{"textLength", "lengthAdjust", "rotate"}

type textLayout struct {
	d       *decoder
	chunks  []TextChunk
	pending vec
	penY    float64
	space   bool
	label   strings.Builder
	label32 int32
}

func (d *decoder) text(n *svgNode, st renderState) error {
	x, err := d.singleCoordinate(n, "x", st)
	if err != nil {
		return err
	}
	y, err := d.singleCoordinate(n, "y", st)
	if err != nil {
		return err
	}
	label, err := listIndex(len(d.list.Labels))
	if err != nil {
		return err
	}
	l := &textLayout{d: d, space: true, label32: label}
	l.newChunk(vec{x, y}, st.style.anchor)
	if err := l.shift(n, st); err != nil {
		return err
	}
	if err := l.content(n, st); err != nil {
		return err
	}
	l.trimTrailing()
	d.list.Labels = append(d.list.Labels, strings.TrimSpace(l.label.String()))
	for _, c := range l.chunks {
		if len(c.Runs) == 0 {
			continue
		}
		d.list.Chunks = append(d.list.Chunks, c)
		index, err := listIndex(len(d.list.Chunks) - 1)
		if err != nil {
			return err
		}
		d.list.Ops = append(d.list.Ops, Op{Kind: OpText, Index: index})
	}
	return nil
}

func (d *decoder) singleCoordinate(n *svgNode, name string, st renderState) (float64, error) {
	raw, ok := n.attr(name)
	if !ok {
		return 0, nil
	}
	fields := strings.FieldsFunc(raw, isListSeparator)
	if len(fields) != 1 {
		return 0, failAt(FailureInvalid, n.span, "<%s> %s lists %d positions; the static SVG profile positions each text span once", n.name, name, len(fields))
	}
	l, ok := parseLength(fields[0])
	percent := st.viewport.x
	if name == "y" || name == "dy" {
		percent = st.viewport.y
	}
	v := l.resolve(d.basis(st, percent))
	if !ok || !withinLimit(v) {
		return 0, failAt(FailureInvalid, n.span, "<%s> %s %s is malformed or exceeds the coordinate limit", n.name, name, quoted(raw))
	}
	return v, nil
}

func (l *textLayout) newChunk(origin vec, anchor Anchor) {
	if len(l.chunks) > 0 {
		l.label.WriteByte('\n')
		l.space = true
	}
	l.chunks = append(l.chunks, TextChunk{Origin: origin.point(), Anchor: anchor, Label: l.label32})
	l.penY = origin.y
}

func (l *textLayout) shift(n *svgNode, st renderState) error {
	for _, name := range refusedTextAttributes {
		if _, ok := n.attr(name); ok {
			return failAt(FailureInvalid, n.span, "<%s> %s is outside the static SVG profile", n.name, name)
		}
	}
	dx, err := l.d.singleCoordinate(n, "dx", st)
	if err != nil {
		return err
	}
	dy, err := l.d.singleCoordinate(n, "dy", st)
	if err != nil {
		return err
	}
	l.pending = l.pending.add(vec{dx, dy})
	l.penY += dy
	return nil
}

func (l *textLayout) content(n *svgNode, st renderState) error {
	for _, c := range n.children {
		if err := l.child(c, st); err != nil {
			return err
		}
	}
	return nil
}

func (l *textLayout) child(c *svgNode, st renderState) error {
	if c.isText {
		return l.run(c, st)
	}
	if c.name == "title" || c.name == "desc" {
		return nil
	}
	if c.name != "tspan" {
		return failAt(FailureInvalid, c.span, "<%s> inside text is outside the static SVG profile", c.name)
	}
	cs, err := l.d.computeStyle(c, st.style)
	if err != nil || !cs.display {
		return err
	}
	inner := st
	inner.style = cs
	if err := l.position(c, inner); err != nil {
		return err
	}
	if err := l.shift(c, inner); err != nil {
		return err
	}
	return l.content(c, inner)
}

func (l *textLayout) position(c *svgNode, st renderState) error {
	_, hasX := c.attr("x")
	_, hasY := c.attr("y")
	if !hasX && !hasY {
		return nil
	}
	if !hasX {
		return failAt(FailureInvalid, c.span, "<tspan> with y but no x is outside the static SVG profile; give it an x position")
	}
	x, err := l.d.singleCoordinate(c, "x", st)
	if err != nil {
		return err
	}
	y := l.penY
	if hasY {
		if y, err = l.d.singleCoordinate(c, "y", st); err != nil {
			return err
		}
	}
	l.trimTrailing()
	l.pending = vec{}
	l.newChunk(vec{x, y}, st.style.anchor)
	return nil
}

func (l *textLayout) run(c *svgNode, st renderState) error {
	text := l.collapse(c.text, st.style.preserve || c.parent.preservesSpace())
	if text == "" {
		return nil
	}
	s := st.style
	if s.stroke.kind != paintNone {
		return failAt(FailureInvalid, c.parent.span, "stroked text is outside the static SVG profile")
	}
	fill, err := l.fill(c.parent, st)
	if err != nil {
		return err
	}
	baseline := s.baseline
	if s.alignSet {
		baseline = s.alignBaseline
	}
	l.label.WriteString(text)
	chunk := &l.chunks[len(l.chunks)-1]
	chunk.Runs = append(chunk.Runs, TextRun{
		Text: text, Size: float32(s.fontSize), Weight: s.fontWeight, Italic: s.italic,
		Face: s.face, Fill: fill, Shift: l.pending.point(), Baseline: baseline,
	})
	l.pending = vec{}
	return nil
}

func (l *textLayout) fill(n *svgNode, st renderState) (Paint, error) {
	s := st.style
	if s.fill.kind == paintURL {
		return Paint{}, failAt(FailureInvalid, n.span, "gradient-filled text is outside the static SVG profile; use a solid fill")
	}
	paint, err := l.d.resolvePaint(n, s.fill, s.fillOpacity, nil, st)
	if err != nil {
		return Paint{}, err
	}
	if paint.none || !s.visible {
		return SolidPaint(Color{}), nil
	}
	return SolidPaint(paint.color.color()), nil
}

func (l *textLayout) collapse(raw string, preserve bool) string {
	var b strings.Builder
	for _, r := range raw {
		switch r {
		case '\n', '\r':
			if preserve {
				b.WriteRune(' ')
			}
			continue
		case '\t':
			r = ' '
		}
		if r == ' ' && !preserve {
			if l.space {
				continue
			}
			l.space = true
			b.WriteRune(r)
			continue
		}
		l.space = false
		b.WriteRune(r)
	}
	return b.String()
}

func (l *textLayout) trimTrailing() {
	chunk := &l.chunks[len(l.chunks)-1]
	for i := len(chunk.Runs) - 1; i >= 0; i-- {
		trimmed := strings.TrimRight(chunk.Runs[i].Text, " ")
		if trimmed != "" {
			chunk.Runs[i].Text = trimmed
			return
		}
		chunk.Runs = chunk.Runs[:i]
	}
}
