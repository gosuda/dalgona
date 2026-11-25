package figure

import (
	"math"
	"strconv"
	"strings"
)

type matrix struct{ a, b, c, d, e, f float64 }

var identity = matrix{a: 1, d: 1}

func (m matrix) mul(n matrix) matrix {
	return matrix{
		a: m.a*n.a + m.c*n.b,
		b: m.b*n.a + m.d*n.b,
		c: m.a*n.c + m.c*n.d,
		d: m.b*n.c + m.d*n.d,
		e: m.a*n.e + m.c*n.f + m.e,
		f: m.b*n.e + m.d*n.f + m.f,
	}
}

func (m matrix) apply(x, y float64) (float64, float64) {
	return m.a*x + m.c*y + m.e, m.b*x + m.d*y + m.f
}

func (m matrix) det() float64 { return m.a*m.d - m.b*m.c }

func (m matrix) invert() (matrix, bool) {
	det := m.det()
	if det == 0 || math.IsNaN(det) || math.IsInf(det, 0) {
		return matrix{}, false
	}
	return matrix{
		a: m.d / det, b: -m.b / det, c: -m.c / det, d: m.a / det,
		e: (m.c*m.f - m.d*m.e) / det,
		f: (m.b*m.e - m.a*m.f) / det,
	}, true
}

func (m matrix) isIdentity() bool { return m == identity }

func (m matrix) affine() Affine {
	return Affine{A: float32(m.a), B: float32(m.b), C: float32(m.c), D: float32(m.d), E: float32(m.e), F: float32(m.f)}
}

func (m matrix) finite() bool {
	for _, v := range [...]float64{m.a, m.b, m.c, m.d, m.e, m.f} {
		if !withinLimit(v) {
			return false
		}
	}
	return true
}

func translate(x, y float64) matrix { return matrix{a: 1, d: 1, e: x, f: y} }
func scale(x, y float64) matrix     { return matrix{a: x, d: y} }

func rotate(deg float64) matrix {
	s, c := math.Sincos(deg * math.Pi / 180)
	return matrix{a: c, b: s, c: -s, d: c}
}

func withinLimit(v float64) bool {
	return !math.IsNaN(v) && v >= -CoordinateLimit && v <= CoordinateLimit
}

type scanner struct {
	s   string
	pos int
}

func (sc *scanner) skipSpace() {
	for sc.pos < len(sc.s) && isSpace(sc.s[sc.pos]) {
		sc.pos++
	}
}

func (sc *scanner) skipSeparator() {
	sc.skipSpace()
	if sc.pos < len(sc.s) && sc.s[sc.pos] == ',' {
		sc.pos++
		sc.skipSpace()
	}
}

func (sc *scanner) done() bool {
	sc.skipSpace()
	return sc.pos >= len(sc.s)
}

func isSpace(c byte) bool { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' }

func isDigit(c byte) bool { return c >= '0' && c <= '9' }

func isListSeparator(r rune) bool { return r == ',' || r == ' ' || r == '\t' || r == '\n' }

func (sc *scanner) number() (float64, bool) {
	sc.skipSpace()
	start := sc.pos
	end := numberEnd(sc.s, start)
	if end == start {
		return 0, false
	}
	v, err := strconv.ParseFloat(sc.s[start:end], 64)
	if err != nil || math.IsInf(v, 0) {
		return 0, false
	}
	sc.pos = end
	return v, true
}

func numberEnd(s string, start int) int {
	whole := signEnd(s, start)
	i := digitsEnd(s, whole)
	digits := i - whole
	if i < len(s) && s[i] == '.' {
		fraction := i + 1
		i = digitsEnd(s, fraction)
		digits += i - fraction
	}
	if digits == 0 {
		return start
	}
	return exponentEnd(s, i)
}

func exponentEnd(s string, i int) int {
	if i >= len(s) || (s[i] != 'e' && s[i] != 'E') {
		return i
	}
	digits := signEnd(s, i+1)
	end := digitsEnd(s, digits)
	if end == digits {
		return i
	}
	return end
}

func signEnd(s string, i int) int {
	if i < len(s) && (s[i] == '+' || s[i] == '-') {
		return i + 1
	}
	return i
}

func digitsEnd(s string, i int) int {
	for i < len(s) && isDigit(s[i]) {
		i++
	}
	return i
}

func parseNumbers(s string, limit int) ([]float64, bool) {
	sc := scanner{s: s}
	var out []float64
	for !sc.done() {
		v, ok := sc.number()
		if !ok || len(out) >= limit {
			return nil, false
		}
		out = append(out, v)
		sc.skipSpace()
		if sc.pos < len(sc.s) && sc.s[sc.pos] == ',' {
			sc.pos++
			if sc.done() {
				return nil, false
			}
		}
	}
	return out, true
}

type unit uint8

const (
	unitNone unit = iota
	unitPx
	unitPt
	unitPc
	unitIn
	unitCm
	unitMm
	unitEm
	unitEx
	unitPercent
)

var unitNames = map[string]unit{
	"": unitNone, "px": unitPx, "pt": unitPt, "pc": unitPc, "in": unitIn,
	"cm": unitCm, "mm": unitMm, "em": unitEm, "ex": unitEx, "%": unitPercent,
}

type length struct {
	value float64
	unit  unit
}

func parseLength(s string) (length, bool) {
	sc := scanner{s: strings.TrimSpace(s)}
	v, ok := sc.number()
	if !ok {
		return length{}, false
	}
	u, known := unitNames[strings.ToLower(sc.s[sc.pos:])]
	if !known {
		return length{}, false
	}
	return length{value: v, unit: u}, true
}

type lengthBasis struct {
	fontSize float64
	xHeight  float64
	percent  float64
}

func (l length) resolve(basis lengthBasis) float64 {
	switch l.unit {
	case unitEm:
		return l.value * basis.fontSize
	case unitEx:
		return l.value * basis.fontSize * basis.xHeight
	case unitPercent:
		return l.value / 100 * basis.percent
	case unitNone, unitPx, unitPt, unitPc, unitIn, unitCm, unitMm:
	}
	return l.absolute()
}

func (l length) absolute() float64 {
	switch l.unit {
	case unitPt:
		return l.value * 96 / 72
	case unitPc:
		return l.value * 16
	case unitIn:
		return l.value * 96
	case unitCm:
		return l.value * 96 / 2.54
	case unitMm:
		return l.value * 96 / 25.4
	case unitNone, unitPx, unitEm, unitEx, unitPercent:
	}
	return l.value
}

func parseTransform(s string) (matrix, bool) {
	sc := scanner{s: s}
	m := identity
	for !sc.done() {
		next, ok := sc.transformFunction()
		if !ok {
			return matrix{}, false
		}
		m = m.mul(next)
		sc.skipSeparator()
	}
	return m, m.finite()
}

var transformArity = map[string][2]int{
	"matrix": {6, 6}, "translate": {1, 2}, "scale": {1, 2}, "rotate": {1, 3}, "skewX": {1, 1}, "skewY": {1, 1},
}

func (sc *scanner) transformFunction() (matrix, bool) {
	sc.skipSpace()
	open := strings.IndexByte(sc.s[sc.pos:], '(')
	if open < 0 {
		return matrix{}, false
	}
	name := strings.TrimSpace(sc.s[sc.pos : sc.pos+open])
	closing := strings.IndexByte(sc.s[sc.pos+open:], ')')
	if closing < 0 {
		return matrix{}, false
	}
	args, ok := parseNumbers(sc.s[sc.pos+open+1:sc.pos+open+closing], 6)
	sc.pos += open + closing + 1
	arity, known := transformArity[name]
	if !ok || !known || len(args) < arity[0] || len(args) > arity[1] || (name == "rotate" && len(args) == 2) {
		return matrix{}, false
	}
	return transformOf(name, args), true
}

func transformOf(name string, v []float64) matrix {
	switch name {
	case "matrix":
		return matrix{a: v[0], b: v[1], c: v[2], d: v[3], e: v[4], f: v[5]}
	case "translate":
		return translate(v[0], optional(v, 1, 0))
	case "scale":
		return scale(v[0], optional(v, 1, v[0]))
	case "rotate":
		cx, cy := optional(v, 1, 0), optional(v, 2, 0)
		return translate(cx, cy).mul(rotate(v[0])).mul(translate(-cx, -cy))
	case "skewX":
		return matrix{a: 1, c: math.Tan(v[0] * math.Pi / 180), d: 1}
	}
	return matrix{a: 1, b: math.Tan(v[0] * math.Pi / 180), d: 1}
}

func optional(v []float64, i int, fallback float64) float64 {
	if i < len(v) {
		return v[i]
	}
	return fallback
}
