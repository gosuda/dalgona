package secrets

import (
	"context"
	"errors"
	"io"
	"regexp/syntax"
	"unicode/utf8"
)

const scanBlockBytes = 32 << 10

type RegexpReader struct {
	input    regexpInput
	program  *syntax.Prog
	active   []regexpThread
	pending  []regexpThread
	branches []int
	seen     []uint64
	step     uint64
	offset   int64
	previous rune
	lastEnd  int64
}

type regexpThread struct {
	pc    int
	start int64
}

type regexpMatch struct {
	start, end int64
	previous   rune
}

func NewRegexpReader(ctx context.Context, input *io.SectionReader, expression string) (*RegexpReader, error) {
	prog, err := compileRegexpProgram(expression)
	if err != nil {
		return nil, err
	}
	return &RegexpReader{
		input:   regexpInput{ctx: ctx, source: input, base: -1},
		program: prog, previous: -1, lastEnd: -1,
		active:  make([]regexpThread, 0, len(prog.Inst)),
		pending: make([]regexpThread, 0, len(prog.Inst)),
		seen:    make([]uint64, len(prog.Inst)),
	}, nil
}

func compileRegexpProgram(expression string) (*syntax.Prog, error) {
	parsed, err := syntax.Parse(expression, syntax.Perl)
	if err != nil {
		return nil, err
	}
	return syntax.Compile(parsed.Simplify())
}

func (m *RegexpReader) Next() (start, end int64, err error) {
	for m.offset <= m.input.source.Size() {
		match, err := m.find()
		if err != nil {
			return 0, 0, err
		}
		accept := match.start != match.end || match.end != m.lastEnd
		m.lastEnd = match.end
		if err := m.resume(match); err != nil {
			return 0, 0, err
		}
		if accept {
			return match.start, match.end, nil
		}
	}
	return 0, 0, io.EOF
}

func (m *RegexpReader) resume(match regexpMatch) error {
	m.offset, m.previous = match.end, match.previous
	if match.start != match.end {
		return nil
	}
	value, width, err := m.input.at(match.end)
	m.offset += int64(max(width, 1))
	m.previous = value
	return err
}

func (m *RegexpReader) find() (regexpMatch, error) {
	pos, previous := m.offset, m.previous
	match := regexpMatch{start: -1}
	m.pending = m.pending[:0]
	for {
		if err := m.input.ctx.Err(); err != nil {
			return regexpMatch{}, context.Cause(m.input.ctx)
		}
		value, width, err := m.input.at(pos)
		if err != nil {
			return regexpMatch{}, err
		}
		m.expand(pos, syntax.EmptyOpContext(previous, value), match.start < 0)
		m.consume(value, pos, previous, &match)
		if width == 0 || match.start >= 0 && len(m.pending) == 0 {
			break
		}
		pos += int64(width)
		previous = value
	}
	if match.start < 0 {
		return regexpMatch{}, io.EOF
	}
	return match, nil
}

func (m *RegexpReader) expand(pos int64, gap syntax.EmptyOp, begin bool) {
	m.step++
	m.active = m.active[:0]
	for _, thread := range m.pending {
		m.follow(thread, gap)
	}
	if begin {
		m.follow(regexpThread{pc: m.program.Start, start: pos}, gap)
	}
}

func (m *RegexpReader) follow(thread regexpThread, gap syntax.EmptyOp) {
	pc := thread.pc
	for {
		for pc != 0 && m.seen[pc] != m.step {
			m.seen[pc] = m.step
			pc = m.visit(regexpThread{pc: pc, start: thread.start}, gap)
		}
		if len(m.branches) == 0 {
			return
		}
		last := len(m.branches) - 1
		pc = m.branches[last]
		m.branches = m.branches[:last]
	}
}

func (m *RegexpReader) visit(thread regexpThread, gap syntax.EmptyOp) int {
	inst := &m.program.Inst[thread.pc]
	switch inst.Op {
	case syntax.InstAlt, syntax.InstAltMatch:
		m.branches = append(m.branches, int(inst.Arg))
		return int(inst.Out)
	case syntax.InstEmptyWidth:
		if inst.Arg&uint32(gap) == inst.Arg {
			return int(inst.Out)
		}
	case syntax.InstCapture, syntax.InstNop:
		return int(inst.Out)
	case syntax.InstFail:
	case syntax.InstMatch, syntax.InstRune, syntax.InstRune1, syntax.InstRuneAny,
		syntax.InstRuneAnyNotNL:
		m.active = append(m.active, thread)
	default:
		m.active = append(m.active, thread)
	}
	return 0
}

func (m *RegexpReader) consume(value rune, pos int64, previous rune, match *regexpMatch) {
	m.pending = m.pending[:0]
	for _, thread := range m.active {
		inst := &m.program.Inst[thread.pc]
		if inst.Op == syntax.InstMatch {
			*match = regexpMatch{start: thread.start, end: pos, previous: previous}
			return
		}
		if value >= 0 && consumesRune(inst, value) {
			m.pending = append(m.pending, regexpThread{pc: int(inst.Out), start: thread.start})
		}
	}
}

func consumesRune(inst *syntax.Inst, value rune) bool {
	switch inst.Op {
	case syntax.InstRuneAny:
		return true
	case syntax.InstRuneAnyNotNL:
		return value != '\n'
	case syntax.InstRune1:
		return value == inst.Rune[0]
	case syntax.InstRune:
		return inst.MatchRune(value)
	case syntax.InstAlt, syntax.InstAltMatch, syntax.InstCapture, syntax.InstEmptyWidth,
		syntax.InstMatch, syntax.InstFail, syntax.InstNop:
		return false
	default:
		return inst.MatchRune(value)
	}
}

type regexpInput struct {
	ctx    context.Context
	source *io.SectionReader
	base   int64
	data   [scanBlockBytes + utf8.UTFMax]byte
	count  int
}

func (r *regexpInput) at(offset int64) (rune, int, error) {
	if offset >= r.source.Size() {
		return -1, 0, context.Cause(r.ctx)
	}
	if r.base < 0 || offset < r.base || offset-r.base >= scanBlockBytes {
		if err := r.load(offset); err != nil {
			return 0, 0, err
		}
	}
	value, width := utf8.DecodeRune(r.data[offset-r.base : r.count])
	return value, width, nil
}

func (r *regexpInput) load(offset int64) error {
	if err := context.Cause(r.ctx); err != nil {
		return err
	}
	base := offset / scanBlockBytes * scanBlockBytes
	window := r.data[:min(int64(len(r.data)), r.source.Size()-base)]
	n, err := r.source.ReadAt(window, base)
	if err != nil && !errors.Is(err, io.EOF) {
		return err
	}
	if n != len(window) {
		return io.ErrUnexpectedEOF
	}
	r.base, r.count = base, n
	return nil
}
