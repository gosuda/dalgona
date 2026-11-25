package math

import stdmath "math"

type assemblyPlan struct {
	glyphs  []uint32
	offsets []float32
	size    float32
}

func planAssembly(parts []assemblyPart, target, minOverlap float32, limit int) assemblyPlan {
	return arrange(sequence(parts, repetitions(parts, target, minOverlap, limit)), target, minOverlap)
}

func repetitions(parts []assemblyPart, target, minOverlap float32, limit int) int {
	extenders := countExtenders(parts)
	if extenders == 0 {
		return 0
	}
	most := (limit - (len(parts) - extenders)) / extenders
	if most < 1 || longest(sequence(parts, 0), minOverlap) >= target {
		return 0
	}
	first := longest(sequence(parts, 1), minOverlap)
	step := longest(sequence(parts, 2), minOverlap) - first
	if first >= target || step <= 0 {
		return 1
	}
	need := min(most, 1+int(stdmath.Ceil(float64((target-first)/step))))
	if need < most && longest(sequence(parts, need), minOverlap) < target {
		need++
	}
	return need
}

func countExtenders(parts []assemblyPart) int {
	extenders := 0
	for _, part := range parts {
		if part.extender {
			extenders++
		}
	}
	return extenders
}

func sequence(parts []assemblyPart, repeats int) []assemblyPart {
	out := make([]assemblyPart, 0, len(parts)+repeats*len(parts))
	for _, part := range parts {
		copies := 1
		if part.extender {
			copies = repeats
		}
		for range copies {
			out = append(out, part)
		}
	}
	return out
}

func overlapRange(before, after assemblyPart, minOverlap float32) (float32, float32) {
	most := float32(min(before.end, after.start))
	return min(minOverlap, most), most
}

func longest(parts []assemblyPart, minOverlap float32) float32 {
	var size float32
	for i, part := range parts {
		size += float32(part.full)
		if i > 0 {
			least, _ := overlapRange(parts[i-1], part, minOverlap)
			size -= least
		}
	}
	return size
}

func arrange(parts []assemblyPart, target, minOverlap float32) assemblyPlan {
	plan := assemblyPlan{glyphs: make([]uint32, len(parts)), offsets: make([]float32, len(parts))}
	if len(parts) == 0 {
		return plan
	}
	var capacity float32
	for i := 1; i < len(parts); i++ {
		least, most := overlapRange(parts[i-1], parts[i], minOverlap)
		capacity += most - least
	}
	ratio := float32(0)
	if shrink := longest(parts, minOverlap) - target; shrink > 0 && capacity > 0 {
		ratio = min(1, shrink/capacity)
	}
	for i, part := range parts {
		plan.glyphs[i] = part.glyph
		if i > 0 {
			least, most := overlapRange(parts[i-1], part, minOverlap)
			plan.offsets[i] = plan.offsets[i-1] + float32(parts[i-1].full) - (least + (most-least)*ratio)
		}
	}
	plan.size = plan.offsets[len(parts)-1] + float32(parts[len(parts)-1].full)
	return plan
}

func pickVariant(c construction, base uint32, target float32) (uint32, bool) {
	choice := base
	for i := range c.variants {
		variant := c.variant(i)
		choice = variant.glyph
		if float32(variant.advance) >= target {
			return choice, true
		}
	}
	return choice, false
}

func (l *layouter) choose(c construction, glyph uint32, target float32, st style) (uint32, assemblyPlan, bool) {
	scale := l.em(st) / l.font.upem
	choice, fits := pickVariant(c, glyph, target/scale)
	if fits || c.assembly == 0 {
		return choice, assemblyPlan{}, false
	}
	limit := min(maxAssemblyParts, l.budget())
	return 0, planAssembly(c.parts(), target/scale, float32(l.t.minConnectorOverlap), limit), true
}

func (l *layouter) stretchVertical(glyph uint32, target float32, st style) *box {
	c, ok := l.t.verticalConstruction(glyph)
	if !ok {
		return l.glyphBox(glyph, st)
	}
	choice, plan, assembled := l.choose(c, glyph, target, st)
	if !assembled {
		return l.glyphBox(choice, st)
	}
	scale := l.em(st) / l.font.upem
	assembly := &box{italic: l.t.pixels(c.assemblyItalic(), l.scaleAt(l.em(st)))}
	for i, part := range plan.glyphs {
		piece := l.glyphBox(part, st)
		assembly.add(piece, 0, plan.offsets[i]*scale+piece.depth)
		assembly.width = max(assembly.width, piece.width)
	}
	return assembly
}

func (l *layouter) stretchHorizontal(glyph uint32, target float32, st style) *box {
	c, ok := l.t.horizontalConstruction(glyph)
	if !ok {
		return l.glyphBox(glyph, st)
	}
	choice, plan, assembled := l.choose(c, glyph, target, st)
	if !assembled {
		return l.glyphBox(choice, st)
	}
	scale := l.em(st) / l.font.upem
	assembly := &box{width: plan.size * scale}
	for i, part := range plan.glyphs {
		assembly.add(l.glyphBox(part, st), plan.offsets[i]*scale, 0)
	}
	assembly.inkCenter = assembly.width / 2
	return assembly
}

func (l *layouter) centerOnAxis(b *box, st style) *box {
	shift := (b.height-b.depth)/2 - l.value(st, axisHeight)
	centered := &box{width: b.width, italic: b.italic}
	centered.add(b, 0, -shift)
	return centered
}

func (l *layouter) coverTarget(height, depth float32, st style) float32 {
	axis := l.value(st, axisHeight)
	return 2 * max(height-axis, depth+axis)
}

func (l *layouter) delimiter(d Delimiter, target float32, st style) *box {
	b := &box{width: nullDelimiterSpace * l.em(st)}
	glyph, ok := l.font.lookup(d.Value)
	switch {
	case d.Value == 0:
	case !ok:
		b = l.textBox(string(d.Value), st)
	default:
		b = l.centerOnAxis(l.stretchVertical(glyph, target, st), st)
	}
	b.span = d.Span
	return b
}

func appendBox(list, b *box) {
	list.add(b, list.width, 0)
	list.width += b.width
}

func (l *layouter) fence(n *Fence, st style) *box {
	segments := make([]*box, len(n.Segments))
	var height, depth float32
	for i, segment := range n.Segments {
		segments[i] = l.row(segment, st)
		height = max(height, segments[i].height)
		depth = max(depth, segments[i].depth)
	}
	target := l.coverTarget(height, depth, st)
	list := &box{}
	appendBox(list, l.delimiter(n.Left, target, st))
	for i, segment := range segments {
		if i > 0 {
			appendBox(list, l.delimiter(n.Middles[i-1], target, st))
		}
		appendBox(list, segment)
	}
	appendBox(list, l.delimiter(n.Right, target, st))
	return list
}

func (l *layouter) fenced(inner *box, opening, closing rune, st style) *box {
	target := l.coverTarget(inner.height, inner.depth, st)
	list := &box{}
	appendBox(list, l.delimiter(Delimiter{Value: opening}, target, st))
	appendBox(list, inner)
	appendBox(list, l.delimiter(Delimiter{Value: closing}, target, st))
	return list
}

func (l *layouter) sizedDelimiter(n *SizedDelimiter, st style) *box {
	return l.delimiter(n.Delimiter, commandTable[n.Command].amount*l.em(st), st)
}

func (l *layouter) operator(n *Operator, st style) *box {
	if n.Value == 0 {
		return l.uprightRun(n.Name, st)
	}
	glyph, ok := l.font.lookup(n.Value)
	if !ok {
		return l.textBox(string(n.Value), st)
	}
	if st.level == levelDisplay {
		if c, found := l.t.verticalConstruction(glyph); found {
			glyph, _ = pickVariant(c, glyph, float32(l.t.displayOperatorMin))
		}
	}
	return l.centerOnAxis(l.glyphBox(glyph, st), st)
}
