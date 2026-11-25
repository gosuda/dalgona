package figure

import (
	"math"
	"unicode/utf8"
)

type Point struct{ X, Y float32 }

type Rect struct{ Min, Max Point }

func (r Rect) Width() float32  { return r.Max.X - r.Min.X }
func (r Rect) Height() float32 { return r.Max.Y - r.Min.Y }

type Affine struct{ A, B, C, D, E, F float32 }

func Identity() Affine { return Affine{A: 1, D: 1} }

func (m Affine) Apply(p Point) Point {
	return Point{X: m.A*p.X + m.C*p.Y + m.E, Y: m.B*p.X + m.D*p.Y + m.F}
}

func (m Affine) invert() (Affine, bool) {
	det := m.A*m.D - m.B*m.C
	if det == 0 || math.IsNaN(float64(det)) || math.IsInf(float64(det), 0) {
		return Affine{}, false
	}
	inv := Affine{
		A: m.D / det,
		B: -m.B / det,
		C: -m.C / det,
		D: m.A / det,
	}
	inv.E = -(inv.A*m.E + inv.C*m.F)
	inv.F = -(inv.B*m.E + inv.D*m.F)
	return inv, true
}

type Verb uint8

const (
	VerbMove Verb = iota
	VerbLine
	VerbQuad
	VerbCube
	VerbClose
)

var verbPoints = [...]int{VerbMove: 1, VerbLine: 1, VerbQuad: 2, VerbCube: 3, VerbClose: 0}

type Path struct {
	Verbs  []Verb
	Points []Point
}

type Color struct{ R, G, B, A uint8 }

type Spread uint8

const (
	SpreadPad Spread = iota
	SpreadReflect
	SpreadRepeat
)

type Stop struct {
	Offset float32
	Color  Color
}

type Gradient struct {
	Start  Point
	End    Point
	Stops  []Stop
	Spread Spread
	Space  Affine
}

type Paint struct {
	Color    Color
	Gradient int32
}

func SolidPaint(c Color) Paint { return Paint{Color: c, Gradient: -1} }

type Anchor uint8

const (
	AnchorStart Anchor = iota
	AnchorMiddle
	AnchorEnd
)

type Baseline uint8

const (
	BaselineAlphabetic Baseline = iota
	BaselineMiddle
	BaselineCentral
	BaselineHanging
	BaselineTextTop
	BaselineTextBottom
	BaselineIdeographic
	BaselineMathematical
)

type Face uint8

const (
	FaceSans Face = iota
	FaceMono
)

type TextRun struct {
	Text     string
	Size     float32
	Weight   uint16
	Italic   bool
	Face     Face
	Fill     Paint
	Shift    Point
	Baseline Baseline
}

type TextChunk struct {
	Origin Point
	Anchor Anchor
	Label  int32
	Runs   []TextRun
}

type Tile struct {
	Bounds Rect
	Width  int
	Height int
	Pix    []byte
}

type OpKind uint8

const (
	OpTransform OpKind = iota
	OpClip
	OpOpacity
	OpPop
	OpFill
	OpText
	OpImage
)

type Op struct {
	Kind      OpKind
	Index     int32
	Paint     Paint
	Alpha     float32
	Transform Affine
}

type List struct {
	Width     float32
	Height    float32
	Title     string
	Labels    []string
	Paths     []Path
	Gradients []Gradient
	Chunks    []TextChunk
	Tiles     []Tile
	Ops       []Op
}

func (l *List) Operations() int {
	total := len(l.Ops)
	for i := range l.Paths {
		total += len(l.Paths[i].Verbs)
	}
	for i := range l.Chunks {
		total += len(l.Chunks[i].Runs)
	}
	return total
}

func (l *List) Validate() error {
	if !finiteSize(l.Width) || !finiteSize(l.Height) {
		return fail(FailureProtocol, "figure size %gx%g is outside the coordinate limit", l.Width, l.Height)
	}
	if n := l.Operations(); n > OperationLimit {
		return fail(FailureLimit, "figure has %d display operations; the limit is %d", n, OperationLimit)
	}
	checks := []func() error{l.validatePaths, l.validateGradients, l.validateChunks, l.validateTiles, l.validateLabels, l.validateOps}
	for _, check := range checks {
		if err := check(); err != nil {
			return err
		}
	}
	return nil
}

func finiteSize(v float32) bool {
	return !math.IsNaN(float64(v)) && v >= 0 && v <= CoordinateLimit
}

func finiteCoordinate(v float32) bool {
	return !math.IsNaN(float64(v)) && v >= -CoordinateLimit && v <= CoordinateLimit
}

func finitePoint(p Point) bool { return finiteCoordinate(p.X) && finiteCoordinate(p.Y) }

func finiteAffine(m Affine) bool {
	for _, v := range [...]float32{m.A, m.B, m.C, m.D, m.E, m.F} {
		if !finiteCoordinate(v) {
			return false
		}
	}
	return true
}

func (l *List) validatePaths() error {
	for i := range l.Paths {
		if err := validatePath(&l.Paths[i]); err != nil {
			return err
		}
	}
	return nil
}

func validatePath(p *Path) error {
	need := 0
	for _, v := range p.Verbs {
		if int(v) >= len(verbPoints) {
			return fail(FailureProtocol, "path verb %d is not registered", v)
		}
		need += verbPoints[v]
	}
	if need != len(p.Points) {
		return fail(FailureProtocol, "path has %d points for verbs needing %d", len(p.Points), need)
	}
	if len(p.Verbs) > 0 && p.Verbs[0] != VerbMove {
		return fail(FailureProtocol, "path does not start with a move")
	}
	for _, pt := range p.Points {
		if !finitePoint(pt) {
			return fail(FailureProtocol, "path point (%g, %g) is outside the coordinate limit", pt.X, pt.Y)
		}
	}
	return nil
}

func (l *List) validateGradients() error {
	for _, g := range l.Gradients {
		if len(g.Stops) == 0 || g.Spread > SpreadRepeat {
			return fail(FailureProtocol, "gradient has no stops or an unregistered spread")
		}
		if !finitePoint(g.Start) || !finitePoint(g.End) || !finiteAffine(g.Space) {
			return fail(FailureProtocol, "gradient geometry is outside the coordinate limit")
		}
		if err := validateStops(g.Stops); err != nil {
			return err
		}
	}
	return nil
}

func validateStops(stops []Stop) error {
	for i, s := range stops {
		if !(s.Offset >= 0 && s.Offset <= 1) || (i > 0 && s.Offset < stops[i-1].Offset) {
			return fail(FailureProtocol, "gradient stop offsets must rise within 0 and 1")
		}
	}
	return nil
}

func (l *List) validatePaint(p Paint) error {
	if p.Gradient < -1 || int(p.Gradient) >= len(l.Gradients) {
		return fail(FailureProtocol, "paint references gradient %d of %d", p.Gradient, len(l.Gradients))
	}
	return nil
}

func (l *List) validateChunks() error {
	for _, c := range l.Chunks {
		if !finitePoint(c.Origin) || c.Anchor > AnchorEnd || c.Label < -1 || int(c.Label) >= len(l.Labels) {
			return fail(FailureProtocol, "text chunk has an invalid origin, anchor, or label")
		}
		for _, r := range c.Runs {
			if err := l.validateRun(r); err != nil {
				return err
			}
		}
	}
	return nil
}

func (l *List) validateRun(r TextRun) error {
	if !utf8.ValidString(r.Text) || !(r.Size > 0 && r.Size <= CoordinateLimit) || r.Face > FaceMono || r.Baseline > BaselineMathematical {
		return fail(FailureProtocol, "text run has invalid text, size, face, or baseline")
	}
	if r.Weight < 1 || r.Weight > 1000 || !finitePoint(r.Shift) {
		return fail(FailureProtocol, "text run has an invalid weight or shift")
	}
	return l.validatePaint(r.Fill)
}

func (l *List) validateTiles() error {
	for _, t := range l.Tiles {
		if err := validateTile(t); err != nil {
			return err
		}
	}
	return nil
}

func validateTile(t Tile) error {
	if t.Width < 1 || t.Height < 1 || t.Width > TileSideLimit || t.Height > TileSideLimit {
		return fail(FailureProtocol, "tile size %dx%d is outside %d pixels", t.Width, t.Height, TileSideLimit)
	}
	if len(t.Pix) != t.Width*t.Height*4 || len(t.Pix) > TileByteLimit {
		return fail(FailureProtocol, "tile pixel data does not match its size")
	}
	if !finitePoint(t.Bounds.Min) || !finitePoint(t.Bounds.Max) {
		return fail(FailureProtocol, "tile bounds are outside the coordinate limit")
	}
	return nil
}

func (l *List) validateLabels() error {
	if !utf8.ValidString(l.Title) {
		return fail(FailureProtocol, "figure title is not valid UTF-8")
	}
	for _, label := range l.Labels {
		if !utf8.ValidString(label) {
			return fail(FailureProtocol, "figure label is not valid UTF-8")
		}
	}
	return nil
}

func (l *List) validateOps() error {
	depth := 0
	for _, o := range l.Ops {
		if err := l.validateOp(o); err != nil {
			return err
		}
		switch o.Kind {
		case OpTransform, OpClip, OpOpacity:
			depth++
		case OpPop:
			depth--
		case OpFill, OpText, OpImage:
		}
		if depth < 0 {
			return fail(FailureProtocol, "display operations pop an empty stack")
		}
		if depth > StateDepthLimit {
			return fail(FailureLimit, "display operations nest %d states; the limit is %d", depth, StateDepthLimit)
		}
	}
	if depth != 0 {
		return fail(FailureProtocol, "display operations leave %d pushed states", depth)
	}
	return nil
}

func (l *List) validateOp(o Op) error {
	switch o.Kind {
	case OpTransform, OpOpacity, OpPop:
		return validateState(o)
	case OpClip, OpText, OpImage:
		count, what := l.referenceTarget(o.Kind)
		return indexWithin(o.Index, count, what)
	case OpFill:
		if err := indexWithin(o.Index, len(l.Paths), "fill path"); err != nil {
			return err
		}
		return l.validatePaint(o.Paint)
	}
	return fail(FailureProtocol, "display operation %d is not registered", o.Kind)
}

func validateState(o Op) error {
	if o.Kind == OpTransform && !finiteAffine(o.Transform) {
		return fail(FailureProtocol, "transform is not finite or exceeds the coordinate limit")
	}
	if o.Kind == OpOpacity && !(o.Alpha >= 0 && o.Alpha <= 1) {
		return fail(FailureProtocol, "opacity %g is outside 0 and 1", o.Alpha)
	}
	return nil
}

func (l *List) referenceTarget(kind OpKind) (int, string) {
	switch kind {
	case OpClip, OpFill:
		return len(l.Paths), "path"
	case OpText:
		return len(l.Chunks), "text chunk"
	case OpImage:
		return len(l.Tiles), "tile"
	case OpTransform, OpOpacity, OpPop:
	}
	return 0, "state"
}

func indexWithin(i int32, n int, what string) error {
	if i < 0 || int(i) >= n {
		return fail(FailureProtocol, "%s index %d is outside %d entries", what, i, n)
	}
	return nil
}
