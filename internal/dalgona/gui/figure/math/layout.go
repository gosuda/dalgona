package math

import (
	"cmp"
	"errors"
	"fmt"
	stdmath "math"
	"slices"
)

const (
	MaxLayoutItems     = 200_000
	maxAssemblyParts   = 1_024
	nullDelimiterSpace = 0.12
	tagGapEm           = 1
)

type Options struct {
	Size    float32
	Display bool
	Text    TextFace
}

type Glyph struct {
	ID         uint32
	X, Y, Size float32
}

type Rule struct {
	X, Y, Width, Height float32
}

type TextRun struct {
	Text       string
	X, Y, Size float32
}

type Layout struct {
	Width, Ascent, Descent float32
	Glyphs                 []Glyph
	Rules                  []Rule
	Texts                  []TextRun
	font                   *Font
	text                   TextFace
	formula                *Formula
	regions                []region
}

type region struct {
	span                     Span
	left, top, width, height float32
}

var errLayoutBudget = fmt.Errorf("formula layout needs more than %d drawing operations; split the formula", MaxLayoutItems)

type boxKind uint8

const (
	boxList boxKind = iota
	boxGlyph
	boxRule
	boxText
)

type box struct {
	kind          boxKind
	width         float32
	height, depth float32
	italic        float32
	accentX       float32
	hasAccentX    bool
	inkCenter     float32
	glyph         uint32
	size          float32
	text          string
	kids          []placed
	span          Span
}

type placed struct {
	x, y float32
	box  *box
}

func (b *box) add(child *box, x, y float32) {
	b.kids = append(b.kids, placed{x: x, y: y, box: child})
	b.height = max(b.height, child.height+y)
	b.depth = max(b.depth, child.depth-y)
}

type style struct {
	level    uint8
	cramped  bool
	alphabet string
}

const (
	levelDisplay uint8 = iota
	levelText
	levelScript
	levelScriptScript
)

func (s style) superscript() style {
	next := levelScriptScript
	if s.level <= levelText {
		next = levelScript
	}
	s.level = next
	return s
}

func (s style) subscript() style {
	s = s.superscript()
	s.cramped = true
	return s
}

func (s style) numerator() style {
	s.level = min(s.level+1, levelScriptScript)
	return s
}

func (s style) denominator() style {
	s = s.numerator()
	s.cramped = true
	return s
}

func (s style) cramp() style {
	s.cramped = true
	return s
}

type layouter struct {
	font  *Font
	t     *mathTable
	size  float32
	text  TextFace
	items int
}

func (f *Font) Layout(formula *Formula, opts Options) (*Layout, error) {
	if opts.Text == nil {
		return nil, errors.New("math layout needs a text face for text and fallback glyphs")
	}
	if !(opts.Size > 0) {
		return nil, errors.New("math layout needs a positive size")
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	l := &layouter{font: f, t: f.table, size: opts.Size, text: opts.Text}
	st := style{level: levelText}
	if opts.Display {
		st.level = levelDisplay
	}
	root := l.row(formula.Body, st)
	if formula.Tag != nil {
		root = l.tagged(root, formula.Tag)
	}
	if l.items > MaxLayoutItems {
		return nil, errLayoutBudget
	}
	out := &Layout{Width: root.width, Ascent: root.height, Descent: root.depth, font: f, text: opts.Text, formula: formula}
	out.emit(root, 0, 0)
	out.contain()
	slices.SortStableFunc(out.regions, func(a, b region) int { return cmp.Compare(a.span.Start, b.span.End) })
	return out, nil
}

func (l *Layout) emit(b *box, x, y float32) {
	if b.span.End > b.span.Start {
		l.regions = append(l.regions, region{span: b.span, left: x, top: -(y + b.height), width: b.width, height: b.height + b.depth})
	}
	switch b.kind {
	case boxGlyph:
		l.Glyphs = append(l.Glyphs, Glyph{ID: b.glyph, X: x, Y: -y, Size: b.size})
	case boxRule:
		l.Rules = append(l.Rules, Rule{X: x, Y: -(y + b.height), Width: b.width, Height: b.height + b.depth})
	case boxText:
		l.Texts = append(l.Texts, TextRun{Text: b.text, X: x, Y: -y, Size: b.size})
	case boxList:
		for _, kid := range b.kids {
			l.emit(kid.box, x+kid.x, y+kid.y)
		}
	}
}

func (l *Layout) inkBounds() (float32, float32) {
	lo, hi := float32(0), l.Width
	for _, g := range l.Glyphs {
		lo, hi = min(lo, g.X), max(hi, g.X)
	}
	for _, r := range l.Rules {
		lo, hi = min(lo, r.X), max(hi, r.X+r.Width)
	}
	for _, t := range l.Texts {
		lo, hi = min(lo, t.X), max(hi, t.X)
	}
	for _, r := range l.regions {
		lo, hi = min(lo, r.left), max(hi, r.left+r.width)
	}
	return lo, hi
}

func (l *Layout) shiftInkOrigin(lo float32) {
	for i := range l.Glyphs {
		l.Glyphs[i].X -= lo
	}
	for i := range l.Rules {
		l.Rules[i].X -= lo
	}
	for i := range l.Texts {
		l.Texts[i].X -= lo
	}
	for i := range l.regions {
		l.regions[i].left -= lo
	}
}

func (l *Layout) contain() {
	lo, hi := l.inkBounds()
	width := max(l.Width-lo, hi-lo)
	if width < 0 {
		width = 0
	}
	if lo == 0 {
		l.Width = width
		return
	}
	l.shiftInkOrigin(lo)
	l.Width = width
}

func (l *layouter) em(st style) float32 {
	switch st.level {
	case levelScript:
		return l.size * float32(l.t.scriptPercent) / 100
	case levelScriptScript:
		return l.size * float32(l.t.scriptScriptPercent) / 100
	}
	return l.size
}

func (l *layouter) scaleAt(size float32) pixelScale {
	return pixelScale{scale: size / l.font.upem, ppem: int(stdmath.Round(float64(size)))}
}

func (l *layouter) value(st style, c constant) float32 {
	return l.t.pixels(l.t.constant(c), l.scaleAt(l.em(st)))
}

func (l *layouter) budget() int {
	return max(0, MaxLayoutItems+1-l.items)
}

func (l *layouter) glyphBox(glyph uint32, st style) *box {
	l.items++
	glyph = l.scriptGlyph(glyph, st)
	metrics := l.font.metrics(glyph)
	size := l.em(st)
	at := l.scaleAt(size)
	b := &box{
		kind:      boxGlyph,
		glyph:     glyph,
		size:      size,
		width:     metrics.advance * at.scale,
		height:    metrics.ascent * at.scale,
		depth:     metrics.descent * at.scale,
		italic:    l.t.pixels(l.t.italicCorrection(glyph), at),
		inkCenter: (metrics.inkLeft + metrics.inkRight) / 2 * at.scale,
	}
	if attachment, ok := l.t.topAccentAttachment(glyph); ok {
		b.accentX, b.hasAccentX = l.t.pixels(attachment, at), true
	}
	return b
}

func (l *layouter) scriptGlyph(glyph uint32, st style) uint32 {
	if st.level < levelScript {
		return glyph
	}
	return substitute(l.font.scriptStyle, glyph, int(st.level-levelScript))
}

func (l *layouter) textBox(text string, st style) *box {
	l.items++
	size := l.em(st)
	metrics := l.text.MeasureText(text, size)
	return &box{kind: boxText, text: text, size: size, width: metrics.Width, height: metrics.Ascent, depth: metrics.Descent, inkCenter: metrics.Width / 2}
}

func (l *layouter) ruleBox(width, thickness float32) *box {
	l.items++
	return &box{kind: boxRule, width: width, height: thickness}
}

func (l *layouter) runeBox(r rune, st style) *box {
	if glyph, ok := l.font.lookup(styledRune(r, st.alphabet)); ok {
		return l.glyphBox(glyph, st)
	}
	if glyph, ok := l.font.lookup(r); ok {
		return l.glyphBox(glyph, st)
	}
	return l.textBox(string(r), st)
}

func (l *layouter) uprightRun(text string, st style) *box {
	run := &box{}
	for _, r := range text {
		if r == ' ' {
			run.width += 3 * l.em(st) / muPerEm
			continue
		}
		glyph := l.runeBox(r, style{level: st.level, cramped: st.cramped, alphabet: "mathrm"})
		run.add(glyph, run.width, 0)
		run.width += glyph.width + glyph.italic
	}
	return run
}

func tagLabel(tag *Tag) string {
	if tag.Star {
		return tag.Text
	}
	return "(" + tag.Text + ")"
}

func (l *layouter) tagged(body *box, tag *Tag) *box {
	text := l.textBox(tagLabel(tag), style{level: levelText})
	text.span = tag.Span
	root := &box{}
	root.add(body, 0, 0)
	root.add(text, body.width+tagGapEm*l.size, 0)
	root.width = body.width + tagGapEm*l.size + text.width
	return root
}

type atom struct {
	box    *box
	class  Class
	level  uint8
	spaced bool
}

func (l *layouter) row(nodes []Node, st style) *box {
	atoms := make([]atom, 0, len(nodes))
	for _, node := range nodes {
		if change, ok := node.(*StyleChange); ok {
			st = restyle(st, change.Command)
			continue
		}
		b, class := l.atom(node, st)
		b.span = node.Range()
		_, isSpace := node.(*Space)
		atoms = append(atoms, atom{box: b, class: class, level: st.level, spaced: !isSpace})
	}
	reclassify(atoms)
	return l.pack(atoms)
}

var styleLevels = map[string]uint8{"displaystyle": levelDisplay, "textstyle": levelText, "scriptstyle": levelScript, "scriptscriptstyle": levelScriptScript}

func restyle(st style, command string) style {
	st.level = styleLevels[command]
	return st
}

func reclassify(atoms []atom) {
	previous := -1
	for i := range atoms {
		if !atoms[i].spaced {
			continue
		}
		reclassifyPair(atoms, previous, i)
		previous = i
	}
	if previous >= 0 && atoms[previous].class == ClassBin {
		atoms[previous].class = ClassOrd
	}
}

func reclassifyPair(atoms []atom, previous, current int) {
	before := ClassOpen
	if previous >= 0 {
		before = atoms[previous].class
	}
	class := atoms[current].class
	if class == ClassBin && binaryBreaks(before) {
		atoms[current].class = ClassOrd
	}
	if before == ClassBin && closesBinary(class) {
		atoms[previous].class = ClassOrd
	}
}

func closesBinary(class Class) bool {
	return class == ClassRel || class == ClassClose || class == ClassPunct
}

func binaryBreaks(class Class) bool {
	return class == ClassBin || class == ClassOp || class == ClassRel || class == ClassOpen || class == ClassPunct
}

var spacingTable = [8][8]int8{
	{0, 1, -2, -3, 0, 0, 0, -1},
	{1, 1, 0, -3, 0, 0, 0, -1},
	{-2, -2, 0, 0, -2, 0, 0, -2},
	{-3, -3, 0, 0, -3, 0, 0, -3},
	{0, 0, 0, 0, 0, 0, 0, 0},
	{0, 1, -2, -3, 0, 0, 0, -1},
	{-1, -1, 0, -1, -1, -1, -1, -1},
	{-1, 1, -2, -3, -1, 0, -1, -1},
}

var spacingMu = [4]float32{0, 3, 4, 5}

func (l *layouter) spacing(left, right Class, level uint8) float32 {
	entry := spacingTable[left][right]
	if entry < 0 {
		if level > levelText {
			return 0
		}
		entry = -entry
	}
	return spacingMu[entry] * l.em(style{level: level}) / muPerEm
}

func (l *layouter) pack(atoms []atom) *box {
	list := &box{}
	previous := -1
	for i, a := range atoms {
		if a.spaced && previous >= 0 {
			list.width += l.spacing(atoms[previous].class, a.class, a.level)
		}
		list.add(a.box, list.width, 0)
		list.width += a.box.width
		if a.spaced {
			previous = i
		}
	}
	return list
}

func (l *layouter) atom(node Node, st style) (*box, Class) {
	switch n := node.(type) {
	case *Symbol:
		b := l.runeBox(n.Value, st)
		b.width += b.italic
		return b, n.Class
	case *Number:
		return l.number(n.Digits, st), ClassOrd
	case *Group:
		return l.row(n.Body, st), ClassOrd
	case *Scripts:
		return l.scripts(n, st), scriptsClass(n)
	case *Fraction:
		return l.fraction(n, st), ClassInner
	case *Radical:
		return l.radical(n, st), ClassOrd
	case *Operator:
		b := l.operator(n, st)
		b.width += b.italic
		return b, ClassOp
	}
	return l.compound(node, st)
}

func (l *layouter) compound(node Node, st style) (*box, Class) {
	switch n := node.(type) {
	case *Fence:
		return l.fence(n, st), ClassInner
	case *SizedDelimiter:
		return l.sizedDelimiter(n, st), commandTable[n.Command].class
	case *Accent:
		return l.accent(n, st), ClassOrd
	case *Alphabet:
		st.alphabet = n.Command
		return l.row(n.Body, st), ClassOrd
	case *Text:
		return l.textBox(n.Value, st), ClassOrd
	case *Space:
		return l.space(n, st), ClassOrd
	case *Environment:
		return l.environment(n, st)
	}
	return &box{}, ClassOrd
}

func scriptsClass(n *Scripts) Class {
	switch base := n.Base.(type) {
	case *Symbol:
		return base.Class
	case *Operator:
		return ClassOp
	case *Fraction, *Fence:
		return ClassInner
	case *SizedDelimiter:
		return commandTable[base.Command].class
	}
	return ClassOrd
}

func (l *layouter) number(digits string, st style) *box {
	run := &box{}
	for _, r := range digits {
		glyph := l.runeBox(r, st)
		run.add(glyph, run.width, 0)
		run.width += glyph.width
	}
	return run
}

func (l *layouter) space(n *Space, st style) *box {
	amount := n.Amount * l.em(st)
	if n.Unit == "mu" {
		amount /= muPerEm
	}
	return &box{width: amount}
}
