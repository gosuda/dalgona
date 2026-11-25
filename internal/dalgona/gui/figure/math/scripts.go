package math

import (
	stdmath "math"
	"strings"
)

type limitGaps struct {
	upperGap, upperRise float32
	lowerGap, lowerDrop float32
}

type cutIn struct {
	base, script corner
}

var primeRunes = [...]rune{'′', '″', '‴', '⁗'}

func (l *layouter) nucleus(node Node, st style) (*box, bool) {
	switch n := node.(type) {
	case nil:
		return &box{}, false
	case *Symbol:
		b := l.runeBox(n.Value, st)
		b.span = n.Span
		return b, b.kind == boxGlyph && !l.t.extendedShape(b.glyph)
	case *Operator:
		b := l.operator(n, st)
		b.span = n.Span
		return b, false
	}
	b, _ := l.atom(node, st)
	b.span = node.Range()
	return b, false
}

func (l *layouter) scripts(n *Scripts, st style) *box {
	base, glyphBase := l.nucleus(n.Base, st)
	sup := l.superscriptBox(n, st)
	var sub *box
	if n.Sub != nil {
		sub = l.row(n.Sub.Body, st.subscript())
	}
	if gaps, stacked := l.stacking(n.Base, st); stacked {
		return l.limits(base, sup, sub, gaps)
	}
	return l.attach(base, glyphBase, sup, sub, st)
}

func (l *layouter) superscriptBox(n *Scripts, st style) *box {
	if n.Primes == 0 && n.Sup == nil {
		return nil
	}
	nodes := primeNodes(n.Primes)
	if n.Sup != nil {
		nodes = append(nodes, n.Sup.Body...)
	}
	return l.row(nodes, st.superscript())
}

func primeNodes(count int) []Node {
	if count > 0 && count <= len(primeRunes) {
		return []Node{&Symbol{Value: primeRunes[count-1], Class: ClassOrd}}
	}
	nodes := make([]Node, count)
	for i := range nodes {
		nodes[i] = &Symbol{Value: primeRunes[0], Class: ClassOrd}
	}
	return nodes
}

func takesLimits(op *Operator) bool {
	switch {
	case op.Command == "":
		return largeOperatorLimits[op.Value]
	case strings.HasPrefix(op.Command, "operatorname"):
		return op.Command == "operatorname*"
	}
	return commandTable[op.Command].limits
}

func (l *layouter) stacking(base Node, st style) (limitGaps, bool) {
	gaps := limitGaps{
		upperGap:  l.value(st, upperLimitGapMin),
		upperRise: l.value(st, upperLimitBaselineRiseMin),
		lowerGap:  l.value(st, lowerLimitGapMin),
		lowerDrop: l.value(st, lowerLimitBaselineDropMin),
	}
	switch n := base.(type) {
	case *Operator:
		above := n.Limits == LimitsAbove || n.Limits == LimitsDefault && st.level == levelDisplay && takesLimits(n)
		return gaps, above
	case *Accent:
		switch commandTable[n.Command].form {
		case accentOverBrace:
			gaps.upperGap, gaps.upperRise = l.value(st, stretchStackGapAboveMin), l.value(st, stretchStackTopShiftUp)
			return gaps, true
		case accentUnderBrace:
			gaps.lowerGap, gaps.lowerDrop = l.value(st, stretchStackGapBelowMin), l.value(st, stretchStackBottomShiftDown)
			return gaps, true
		case accentGlyph, accentWide, accentOverline, accentUnderline:
		}
	}
	return gaps, false
}

func (l *layouter) limits(base, sup, sub *box, gaps limitGaps) *box {
	width := base.width
	if sup != nil {
		width = max(width, sup.width)
	}
	if sub != nil {
		width = max(width, sub.width)
	}
	stack := &box{width: width}
	stack.add(base, (width-base.width)/2, 0)
	if sup != nil {
		stack.add(sup, (width-sup.width+base.italic)/2, base.height+max(gaps.upperRise, gaps.upperGap+sup.depth))
	}
	if sub != nil {
		stack.add(sub, (width-sub.width-base.italic)/2, -(base.depth + max(gaps.lowerDrop, gaps.lowerGap+sub.height)))
	}
	return stack
}

func (l *layouter) attach(base *box, glyphBase bool, sup, sub *box, st style) *box {
	var supShift, subShift float32
	if !glyphBase {
		supShift = base.height - l.value(st, superscriptBaselineDropMax)
		subShift = base.depth + l.value(st, subscriptBaselineDropMin)
	}
	if sup != nil {
		shiftUp := superscriptShiftUp
		if st.cramped {
			shiftUp = superscriptShiftUpCramped
		}
		supShift = max(supShift, l.value(st, shiftUp), sup.depth+l.value(st, superscriptBottomMin))
	}
	if sub != nil {
		subShift = max(subShift, l.value(st, subscriptShiftDown), sub.height-l.value(st, subscriptTopMax))
	}
	if sup != nil && sub != nil {
		supShift, subShift = l.separate(supShift, subShift, sup, sub, st)
	}
	out := &box{}
	out.add(base, 0, 0)
	width := base.width
	if sup != nil {
		x := base.width + base.italic + l.kern(base, sup, cutIn{cornerTopRight, cornerBottomLeft}, [2]float32{base.height, supShift - sup.depth}, supShift)
		out.add(sup, x, supShift)
		width = max(width, x+sup.width)
	}
	if sub != nil {
		x := base.width + l.kern(base, sub, cutIn{cornerBottomRight, cornerTopLeft}, [2]float32{-base.depth, sub.height - subShift}, -subShift)
		out.add(sub, x, -subShift)
		width = max(width, x+sub.width)
	}
	out.width = width + l.value(st, spaceAfterScript)
	return out
}

func (l *layouter) separate(supShift, subShift float32, sup, sub *box, st style) (float32, float32) {
	gap := (supShift - sup.depth) - (sub.height - subShift)
	least := l.value(st, subSuperscriptGapMin)
	if gap >= least {
		return supShift, subShift
	}
	deficit := least - gap
	room := l.value(st, superscriptBottomMaxWithSubscript) - (supShift - sup.depth)
	up := max(0, min(deficit, room))
	return supShift + up, subShift + deficit - up
}

func leadingGlyph(b *box) *box {
	for b.kind == boxList && len(b.kids) > 0 && b.kids[0].x == 0 && b.kids[0].y == 0 {
		b = b.kids[0].box
	}
	if b.kind != boxGlyph {
		return nil
	}
	return b
}

func (l *layouter) kern(base, script *box, corners cutIn, heights [2]float32, shift float32) float32 {
	if base.kind != boxGlyph {
		return 0
	}
	lead := leadingGlyph(script)
	best := float32(stdmath.Inf(1))
	for _, height := range heights {
		amount := l.glyphKern(base, corners.base, height)
		if lead != nil {
			amount += l.glyphKern(lead, corners.script, height-shift)
		}
		best = min(best, amount)
	}
	return best
}

func (l *layouter) glyphKern(glyph *box, at corner, height float32) float32 {
	return l.t.kern(glyph.glyph, at, height, l.scaleAt(glyph.size))
}

func (l *layouter) pick(st style, display, text constant) float32 {
	if st.level == levelDisplay {
		return l.value(st, display)
	}
	return l.value(st, text)
}

func fractionStyle(command string, st style) style {
	switch command {
	case "dfrac":
		st.level = levelDisplay
	case "tfrac":
		st.level = levelText
	}
	return st
}

func (l *layouter) fraction(n *Fraction, st style) *box {
	st = fractionStyle(n.Command, st)
	numerator := l.row(n.Numerator, st.numerator())
	denominator := l.row(n.Denominator, st.denominator())
	if n.Command == "binom" {
		return l.fenced(l.stack(numerator, denominator, st), '(', ')', st)
	}
	thickness := l.value(st, fractionRuleThickness)
	axis := l.value(st, axisHeight)
	up := max(l.pick(st, fractionNumeratorDisplayStyleShiftUp, fractionNumeratorShiftUp),
		axis+thickness/2+l.pick(st, fractionNumDisplayStyleGapMin, fractionNumeratorGapMin)+numerator.depth)
	down := max(l.pick(st, fractionDenominatorDisplayStyleShiftDown, fractionDenominatorShiftDown),
		l.pick(st, fractionDenomDisplayStyleGapMin, fractionDenominatorGapMin)+thickness/2-axis+denominator.height)
	pad := nullDelimiterSpace * l.em(st)
	width := max(numerator.width, denominator.width)
	out := &box{width: width + 2*pad}
	out.add(numerator, pad+(width-numerator.width)/2, up)
	out.add(denominator, pad+(width-denominator.width)/2, -down)
	out.add(l.ruleBox(width, thickness), pad, axis-thickness/2)
	return out
}

func (l *layouter) stack(numerator, denominator *box, st style) *box {
	up := l.pick(st, stackTopDisplayStyleShiftUp, stackTopShiftUp)
	down := l.pick(st, stackBottomDisplayStyleShiftDown, stackBottomShiftDown)
	least := l.pick(st, stackDisplayStyleGapMin, stackGapMin)
	if gap := (up - numerator.depth) - (denominator.height - down); gap < least {
		up += (least - gap) / 2
		down += (least - gap) / 2
	}
	width := max(numerator.width, denominator.width)
	out := &box{width: width}
	out.add(numerator, (width-numerator.width)/2, up)
	out.add(denominator, (width-denominator.width)/2, -down)
	return out
}

func (l *layouter) radical(n *Radical, st style) *box {
	body := l.row(n.Body, st.cramp())
	gap := l.pick(st, radicalDisplayStyleVerticalGap, radicalVerticalGap)
	thickness := l.value(st, radicalRuleThickness)
	target := body.height + body.depth + gap + thickness
	sign := l.radicalSign(target, st)
	if total := sign.height + sign.depth; total > target {
		gap += (total - target) / 2
	}
	ruleBottom := body.height + gap
	signShift := ruleBottom + thickness - sign.height
	out := &box{}
	x := l.radicalDegree(out, n.Index, sign, signShift, st)
	out.add(sign, x, signShift)
	x += sign.width
	out.add(l.ruleBox(body.width, thickness), x, ruleBottom)
	out.add(body, x, 0)
	out.width = x + body.width
	out.height = max(out.height, ruleBottom+thickness+l.value(st, radicalExtraAscender))
	return out
}

func (l *layouter) radicalSign(target float32, st style) *box {
	glyph, ok := l.font.lookup('√')
	if !ok {
		return l.textBox("√", st)
	}
	return l.stretchVertical(glyph, target, st)
}

func (l *layouter) radicalDegree(out *box, index *Field, sign *box, signShift float32, st style) float32 {
	if index == nil {
		return 0
	}
	degree := l.row(index.Body, style{level: levelScriptScript, alphabet: st.alphabet})
	before := l.value(st, radicalKernBeforeDegree)
	signX := before + degree.width + l.value(st, radicalKernAfterDegree)
	offset := max(0, -signX)
	bottom := signShift - sign.depth
	raise := float32(l.t.degreeRaisePercent) / 100 * (sign.height + sign.depth)
	out.add(degree, before+offset, bottom+raise)
	return signX + offset
}

func (l *layouter) accent(n *Accent, st style) *box {
	entry := commandTable[n.Command]
	switch entry.form {
	case accentOverline:
		return l.overline(n.Body, st)
	case accentUnderline:
		return l.underline(n.Body, st)
	case accentOverBrace, accentUnderBrace:
		return l.brace(n.Body, entry, st)
	case accentGlyph, accentWide:
	}
	return l.accentMark(n.Body, entry, st)
}

func (l *layouter) accentBase(body []Node, st style) *box {
	if len(body) == 1 {
		b, _ := l.nucleus(body[0], st)
		return b
	}
	return l.row(body, st)
}

func (l *layouter) accentMark(body []Node, entry command, st style) *box {
	base := l.accentBase(body, st.cramp())
	mark := l.accentGlyph(entry, base, st)
	baseX := base.width / 2
	if base.hasAccentX {
		baseX = base.accentX
	}
	markX := mark.inkCenter
	if mark.hasAccentX {
		markX = mark.accentX
	}
	out := &box{width: base.width, italic: base.italic}
	out.add(base, 0, 0)
	out.add(mark, baseX-markX, max(0, base.height-l.value(st, accentBaseHeight)))
	return out
}

func (l *layouter) accentGlyph(entry command, base *box, st style) *box {
	glyph, ok := l.font.lookup(entry.value)
	switch {
	case !ok:
		return l.textBox(string(entry.value), st)
	case entry.form == accentWide:
		return l.stretchHorizontal(glyph, base.width, st)
	case base.height > l.value(st, flattenedAccentBaseHeight):
		glyph = substitute(l.font.flattenedAccent, glyph, 0)
	}
	return l.glyphBox(glyph, st)
}

func (l *layouter) overline(body []Node, st style) *box {
	base := l.row(body, st.cramp())
	gap := l.value(st, overbarVerticalGap)
	thickness := l.value(st, overbarRuleThickness)
	out := &box{width: base.width}
	out.add(base, 0, 0)
	out.add(l.ruleBox(base.width, thickness), 0, base.height+gap)
	out.height = base.height + gap + thickness + l.value(st, overbarExtraAscender)
	return out
}

func (l *layouter) underline(body []Node, st style) *box {
	base := l.row(body, st)
	gap := l.value(st, underbarVerticalGap)
	thickness := l.value(st, underbarRuleThickness)
	out := &box{width: base.width}
	out.add(base, 0, 0)
	out.add(l.ruleBox(base.width, thickness), 0, -(base.depth + gap + thickness))
	out.depth = base.depth + gap + thickness + l.value(st, underbarExtraDescender)
	return out
}

func (l *layouter) brace(body []Node, entry command, st style) *box {
	base := l.row(body, st)
	glyph, ok := l.font.lookup(entry.value)
	if !ok {
		return base
	}
	mark := l.stretchHorizontal(glyph, base.width, st)
	width := max(base.width, mark.width)
	out := &box{width: width}
	out.add(base, (width-base.width)/2, 0)
	y := base.height + l.value(st, stretchStackGapBelowMin) + mark.depth
	if entry.form == accentUnderBrace {
		y = -(base.depth + l.value(st, stretchStackGapAboveMin) + mark.height)
	}
	out.add(mark, (width-mark.width)/2, y)
	return out
}
