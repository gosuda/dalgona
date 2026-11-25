package figure

import (
	"strings"
)

type pathParser struct {
	sc      scanner
	out     *shape
	command byte
	control vec
	last    byte
	err     string
}

var pathArity = map[byte]int{
	'M': 2, 'L': 2, 'H': 1, 'V': 1, 'C': 6, 'S': 4, 'Q': 4, 'T': 2, 'A': 7, 'Z': 0,
}

func parsePathData(d string) (*shape, string) {
	p := &pathParser{sc: scanner{s: d}, out: &shape{}}
	for !p.sc.done() && p.err == "" {
		p.step()
	}
	return p.out, p.err
}

func (p *pathParser) step() {
	c := p.sc.s[p.sc.pos]
	if _, ok := pathArity[upper(c)]; ok {
		p.sc.pos++
		p.command = c
		if upper(c) == 'Z' {
			p.out.close()
			p.last = 'Z'
			p.sc.skipSeparator()
			return
		}
	} else if p.command == 0 || upper(p.command) == 'Z' {
		p.err = "path data must start with a move command"
		return
	}
	args, ok := p.arguments(pathArity[upper(p.command)], upper(p.command) == 'A')
	if !ok {
		p.err = "path data has a malformed number near byte " + itoa(p.sc.pos)
		return
	}
	p.apply(args)
}

func upper(c byte) byte {
	if c >= 'a' && c <= 'z' {
		return c - 'a' + 'A'
	}
	return c
}

func itoa(v int) string {
	var b [20]byte
	i := len(b)
	for {
		i--
		b[i] = byte('0' + v%10)
		v /= 10
		if v == 0 {
			return string(b[i:])
		}
	}
}

func (p *pathParser) arguments(n int, arc bool) ([7]float64, bool) {
	var args [7]float64
	for i := range n {
		if arc && (i == 3 || i == 4) {
			flag, ok := p.flag()
			if !ok {
				return args, false
			}
			args[i] = flag
		} else {
			v, ok := p.sc.number()
			if !ok || !withinLimit(v) {
				return args, false
			}
			args[i] = v
		}
		p.sc.skipSeparator()
	}
	return args, true
}

func (p *pathParser) flag() (float64, bool) {
	p.sc.skipSpace()
	if p.sc.pos >= len(p.sc.s) {
		return 0, false
	}
	c := p.sc.s[p.sc.pos]
	if c != '0' && c != '1' {
		return 0, false
	}
	p.sc.pos++
	return float64(c - '0'), true
}

func (p *pathParser) apply(a [7]float64) {
	relative := p.command >= 'a'
	origin := vec{}
	if relative {
		origin = p.out.pen
	}
	pt := func(i int) vec { return vec{a[i], a[i+1]}.add(origin) }
	switch upper(p.command) {
	case 'M':
		p.out.moveTo(pt(0))
		p.command = map[bool]byte{true: 'l', false: 'L'}[relative]
	case 'L':
		p.out.lineTo(pt(0))
	case 'H':
		p.out.lineTo(vec{a[0] + origin.x, p.out.pen.y})
	case 'V':
		p.out.lineTo(vec{p.out.pen.x, a[0] + origin.y})
	default:
		p.applyCurve(pt, a)
	}
	p.last = upper(p.command)
}

func (p *pathParser) applyCurve(pt func(int) vec, a [7]float64) {
	switch upper(p.command) {
	case 'C':
		p.control = pt(2)
		p.out.cubeTo(pt(0), p.control, pt(4), true)
	case 'S':
		c1 := p.reflected('C', 'S')
		p.control = pt(0)
		p.out.cubeTo(c1, p.control, pt(2), true)
	case 'Q':
		p.control = pt(0)
		p.out.quadTo(p.control, pt(2))
	case 'T':
		p.control = p.reflected('Q', 'T')
		p.out.quadTo(p.control, pt(0))
	case 'A':
		p.out.arcTo(vec{a[0], a[1]}, a[2], a[3] == 1, a[4] == 1, pt(5))
	}
}

func (p *pathParser) reflected(kinds ...byte) vec {
	pen := p.out.pen
	if strings.IndexByte(string(kinds), p.last) < 0 {
		return pen
	}
	return pen.mul(2).sub(p.control)
}

func parsePoints(s string) ([]vec, bool) {
	nums, ok := parseNumbers(s, 2*SVGElementLimit)
	if !ok || len(nums)%2 != 0 {
		return nil, false
	}
	out := make([]vec, 0, len(nums)/2)
	for i := 0; i < len(nums); i += 2 {
		v := vec{nums[i], nums[i+1]}
		if !v.within() {
			return nil, false
		}
		out = append(out, v)
	}
	return out, true
}
