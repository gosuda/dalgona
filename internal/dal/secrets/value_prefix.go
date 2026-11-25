package secrets

import (
	"math"
	"regexp/syntax"
	"unicode"
	"unicode/utf8"
)

const minValueBytes = 8

type ValuePrefix struct {
	matchers []valueMatcher
	scanned  int64
}

func NewValuePrefix(configured []string, builtin bool) (*ValuePrefix, error) {
	prefix := &ValuePrefix{}
	for _, expression := range configured {
		if err := prefix.add(expression, true); err != nil {
			return nil, err
		}
	}
	if builtin {
		for _, rule := range rules {
			if err := prefix.add(rule.re.String(), false); err != nil {
				return nil, err
			}
		}
	}
	return prefix, nil
}

func (p *ValuePrefix) add(expression string, checkInteriorBytes bool) error {
	prog, err := compileRegexpProgram(expression)
	if err != nil {
		return err
	}
	tail := valueSuffixBytes(prog)
	if tail[prog.Start] >= minValueBytes {
		p.matchers = append(p.matchers, valueMatcher{
			prog: prog, tail: tail, seen: make([]uint64, len(prog.Inst)), step: 1,
			active: make([]regexpThread, 0, len(prog.Inst)), pending: make([]regexpThread, 0, len(prog.Inst)),
			branches: make([]int, 0, len(prog.Inst)), hit: -1, checkInteriorBytes: checkInteriorBytes,
		})
	}
	return nil
}

func (p *ValuePrefix) SafeBytes(window []byte, base int64) int {
	if p.scanned < base || p.scanned-base > int64(len(window)) {
		return 0
	}
	if len(p.matchers) == 0 {
		cut := completeWindowBytes(window)
		p.scanned = base + int64(cut)
		return cut
	}
	delta := p.scanned - base
	for rest := window[delta:]; len(rest) > 0 && utf8.FullRune(rest); {
		r, width := utf8.DecodeRune(rest)
		for i := range p.matchers {
			p.matchers[i].advance(r, width, p.scanned)
		}
		p.scanned += int64(width)
		rest = rest[width:]
	}
	fence := p.scanned
	for i := range p.matchers {
		fence = min(fence, p.matchers[i].fence(fence))
	}
	return int(fence - base)
}

func completeWindowBytes(window []byte) int {
	for i := len(window) - 1; i >= max(0, len(window)-(utf8.UTFMax-1)); i-- {
		if utf8.RuneStart(window[i]) {
			if !utf8.FullRune(window[i:]) {
				return i
			}
			break
		}
	}
	return len(window)
}

type valueMatcher struct {
	prog               *syntax.Prog
	tail               []int
	seen               []uint64
	step               uint64
	active, pending    []regexpThread
	branches           []int
	hit                int64
	checkInteriorBytes bool
}

func (m *valueMatcher) fence(fallback int64) int64 {
	if m.hit >= 0 {
		fallback = min(fallback, m.hit)
	}
	if len(m.active) != 0 {
		fallback = min(fallback, m.active[0].start)
	}
	return fallback
}

func (m *valueMatcher) advance(r rune, width int, pos int64) {
	if m.hit < 0 {
		m.follow(regexpThread{pc: m.prog.Start, start: pos}, pos)
	}
	m.step++
	m.pending, m.active = m.active, m.pending[:0]
	end := pos + int64(width)
	for _, thread := range m.pending {
		inst := &m.prog.Inst[thread.pc]
		if width > 1 && m.checkInteriorBytes && consumesRune(inst, utf8.RuneError) {
			m.checkRuneError(thread, pos, int(inst.Out))
		}
		if consumesRune(inst, r) {
			m.follow(regexpThread{pc: int(inst.Out), start: thread.start}, end)
		}
	}
}

func (m *valueMatcher) checkRuneError(thread regexpThread, pos int64, out int) {
	if m.viable(regexpThread{pc: out, start: thread.start}, pos+1) {
		m.hold(thread.start)
	}
}

func (m *valueMatcher) viable(t regexpThread, pos int64) bool {
	return m.tail[t.pc] >= 0 && pos-t.start+int64(m.tail[t.pc]) >= minValueBytes
}

func (m *valueMatcher) follow(thread regexpThread, pos int64) {
	for {
		for m.seen[thread.pc] != m.step && m.viable(thread, pos) {
			m.seen[thread.pc] = m.step
			thread.pc = m.visit(thread)
		}
		if len(m.branches) == 0 {
			return
		}
		last := len(m.branches) - 1
		thread.pc = m.branches[last]
		m.branches = m.branches[:last]
	}
}

func (m *valueMatcher) visit(t regexpThread) int {
	inst := &m.prog.Inst[t.pc]
	switch inst.Op {
	case syntax.InstAlt, syntax.InstAltMatch:
		m.branches = append(m.branches, int(inst.Arg))
		return int(inst.Out)
	case syntax.InstCapture, syntax.InstEmptyWidth, syntax.InstNop:
		return int(inst.Out)
	case syntax.InstMatch:
		m.hold(t.start)
	case syntax.InstFail:
		return 0
	case syntax.InstRune, syntax.InstRune1, syntax.InstRuneAny, syntax.InstRuneAnyNotNL:
		m.active = append(m.active, t)
	default:
		m.active = append(m.active, t)
	}
	return 0
}

func (m *valueMatcher) hold(start int64) {
	if m.hit < 0 || start < m.hit {
		m.hit = start
	}
}

type valueIncoming struct {
	from, next, width int
}

type valueReach struct {
	tail, head, stack []int
	queued            []bool
	edges             []valueIncoming
}

func valueSuffixBytes(prog *syntax.Prog) []int {
	n := len(prog.Inst)
	r := valueReach{
		tail: make([]int, n), head: make([]int, n), stack: make([]int, 0, n),
		queued: make([]bool, n), edges: make([]valueIncoming, 0, n),
	}
	for i := range r.tail {
		r.tail[i], r.head[i] = -1, -1
	}
	for pc := range prog.Inst {
		r.addInstruction(pc, &prog.Inst[pc])
	}
	for len(r.stack) != 0 {
		last := len(r.stack) - 1
		pc := r.stack[last]
		r.stack = r.stack[:last]
		r.queued[pc] = false
		r.grow(pc)
	}
	return r.tail
}

func (r *valueReach) addInstruction(pc int, inst *syntax.Inst) {
	switch inst.Op {
	case syntax.InstFail:
		return
	case syntax.InstMatch:
		r.queue(pc, 0)
		return
	case syntax.InstAlt, syntax.InstAltMatch:
		r.addEdge(pc, int(inst.Arg), 0)
	case syntax.InstCapture, syntax.InstEmptyWidth, syntax.InstNop, syntax.InstRune,
		syntax.InstRune1, syntax.InstRuneAny, syntax.InstRuneAnyNotNL:
	}
	if width := maxRuneBytes(inst); width >= 0 {
		r.addEdge(pc, int(inst.Out), width)
	}
}

func (r *valueReach) addEdge(from, to, width int) {
	edge := valueIncoming{from: from, next: r.head[to], width: width}
	r.head[to] = len(r.edges)
	r.edges = append(r.edges, edge)
}

func (r *valueReach) grow(pc int) {
	for index := r.head[pc]; index >= 0; {
		edge := r.edges[index]
		index = edge.next
		width := min(minValueBytes, r.tail[pc]+edge.width)
		if r.tail[edge.from] < width {
			r.queue(edge.from, width)
		}
	}
}

func (r *valueReach) queue(pc, width int) {
	r.tail[pc] = width
	if !r.queued[pc] {
		r.queued[pc] = true
		r.stack = append(r.stack, pc)
	}
}

func maxRuneBytes(inst *syntax.Inst) int {
	switch inst.Op {
	case syntax.InstRuneAny, syntax.InstRuneAnyNotNL:
		return utf8.UTFMax
	case syntax.InstRune, syntax.InstRune1:
		return runeClassBytes(inst)
	case syntax.InstAlt, syntax.InstAltMatch, syntax.InstCapture,
		syntax.InstEmptyWidth, syntax.InstNop:
		return 0
	case syntax.InstMatch, syntax.InstFail:
		return -1
	default:
		return 0
	}
}

func runeClassBytes(inst *syntax.Inst) int {
	if len(inst.Rune) == 0 {
		return -1
	}
	if inst.Arg > math.MaxUint16 {
		return -1
	}
	last := len(inst.Rune) - 1
	width := utf8.RuneLen(inst.Rune[last])
	if width < 0 {
		width = utf8.UTFMax
	}
	if last == 0 && syntax.Flags(inst.Arg)&syntax.FoldCase != 0 {
		first := inst.Rune[0]
		for r := unicode.SimpleFold(first); r != first; r = unicode.SimpleFold(r) {
			width = max(width, utf8.RuneLen(r))
		}
	}
	return width
}
