package figure

import (
	"encoding/binary"
	"math"
)

const displayMagic = "FDL1"

func EncodeList(l *List) []byte {
	b := make([]byte, 0, 1024)
	b = append(b, displayMagic...)
	b = appendFloat(b, l.Width)
	b = appendFloat(b, l.Height)
	b = appendString(b, l.Title)
	b = binary.AppendUvarint(b, uint64(len(l.Labels)))
	for _, s := range l.Labels {
		b = appendString(b, s)
	}
	b = binary.AppendUvarint(b, uint64(len(l.Paths)))
	for i := range l.Paths {
		b = appendPath(b, &l.Paths[i])
	}
	b = binary.AppendUvarint(b, uint64(len(l.Gradients)))
	for i := range l.Gradients {
		b = appendGradient(b, &l.Gradients[i])
	}
	b = binary.AppendUvarint(b, uint64(len(l.Chunks)))
	for i := range l.Chunks {
		b = appendChunk(b, &l.Chunks[i])
	}
	b = binary.AppendUvarint(b, uint64(len(l.Tiles)))
	for i := range l.Tiles {
		b = appendTile(b, &l.Tiles[i])
	}
	b = binary.AppendUvarint(b, uint64(len(l.Ops)))
	for _, o := range l.Ops {
		b = appendOp(b, o)
	}
	return b
}

func appendFloat(b []byte, v float32) []byte {
	return binary.LittleEndian.AppendUint32(b, math.Float32bits(v))
}

func appendPoint(b []byte, p Point) []byte { return appendFloat(appendFloat(b, p.X), p.Y) }

func appendString(b []byte, s string) []byte {
	return append(binary.AppendUvarint(b, uint64(len(s))), s...)
}

func appendColor(b []byte, c Color) []byte { return append(b, c.R, c.G, c.B, c.A) }

func appendPaint(b []byte, p Paint) []byte {
	return binary.AppendVarint(appendColor(b, p.Color), int64(p.Gradient))
}

func appendAffine(b []byte, m Affine) []byte {
	for _, v := range [...]float32{m.A, m.B, m.C, m.D, m.E, m.F} {
		b = appendFloat(b, v)
	}
	return b
}

func appendPath(b []byte, p *Path) []byte {
	b = binary.AppendUvarint(b, uint64(len(p.Verbs)))
	for _, v := range p.Verbs {
		b = append(b, byte(v))
	}
	b = binary.AppendUvarint(b, uint64(len(p.Points)))
	for _, pt := range p.Points {
		b = appendPoint(b, pt)
	}
	return b
}

func appendGradient(b []byte, g *Gradient) []byte {
	b = appendPoint(appendPoint(b, g.Start), g.End)
	b = binary.AppendUvarint(b, uint64(len(g.Stops)))
	for _, s := range g.Stops {
		b = appendColor(appendFloat(b, s.Offset), s.Color)
	}
	return appendAffine(append(b, byte(g.Spread)), g.Space)
}

func appendChunk(b []byte, c *TextChunk) []byte {
	b = append(appendPoint(b, c.Origin), byte(c.Anchor))
	b = binary.AppendVarint(b, int64(c.Label))
	b = binary.AppendUvarint(b, uint64(len(c.Runs)))
	for _, r := range c.Runs {
		b = appendString(b, r.Text)
		b = appendFloat(b, r.Size)
		b = binary.AppendUvarint(b, uint64(r.Weight))
		b = append(b, boolByte(r.Italic), byte(r.Face), byte(r.Baseline))
		b = appendPaint(b, r.Fill)
		b = appendPoint(b, r.Shift)
	}
	return b
}

func boolByte(v bool) byte {
	if v {
		return 1
	}
	return 0
}

func appendTile(b []byte, t *Tile) []byte {
	b = appendPoint(appendPoint(b, t.Bounds.Min), t.Bounds.Max)
	b = binary.AppendUvarint(b, uint64(max(t.Width, 0)))
	b = binary.AppendUvarint(b, uint64(max(t.Height, 0)))
	return append(binary.AppendUvarint(b, uint64(len(t.Pix))), t.Pix...)
}

func appendOp(b []byte, o Op) []byte {
	b = append(b, byte(o.Kind))
	switch o.Kind {
	case OpTransform:
		return appendAffine(b, o.Transform)
	case OpOpacity:
		return appendFloat(b, o.Alpha)
	case OpFill:
		return appendPaint(binary.AppendVarint(b, int64(o.Index)), o.Paint)
	case OpPop:
		return b
	case OpClip, OpText, OpImage:
		return binary.AppendVarint(b, int64(o.Index))
	}
	return b
}

type listReader struct {
	buf   []byte
	err   *Failure
	total int
}

const decodedEntryLimit = 3 * OperationLimit

func DecodeList(data []byte) (*List, error) {
	if len(data) > ResultByteLimit {
		return nil, fail(FailureLimit, "display list has %d bytes; the limit is %d", len(data), ResultByteLimit)
	}
	if len(data) < len(displayMagic) || string(data[:len(displayMagic)]) != displayMagic {
		return nil, fail(FailureProtocol, "display list does not start with %s", displayMagic)
	}
	r := &listReader{buf: data[len(displayMagic):]}
	l := r.list()
	if r.err != nil {
		return nil, r.err
	}
	if len(r.buf) != 0 {
		return nil, fail(FailureProtocol, "display list has %d trailing bytes", len(r.buf))
	}
	if err := l.Validate(); err != nil {
		return nil, err
	}
	return l, nil
}

func (r *listReader) list() *List {
	l := &List{Width: r.float(), Height: r.float(), Title: r.string()}
	l.Labels = readSlice(r, 1, (*listReader).string)
	l.Paths = readSlice(r, 2, (*listReader).path)
	l.Gradients = readSlice(r, 34, (*listReader).gradient)
	l.Chunks = readSlice(r, 11, (*listReader).chunk)
	l.Tiles = readSlice(r, 19, (*listReader).tile)
	l.Ops = readSlice(r, 1, (*listReader).op)
	return l
}

func readSlice[T any](r *listReader, minSize int, item func(*listReader) T) []T {
	n := r.count(minSize)
	if n == 0 {
		return nil
	}
	if r.total+n > decodedEntryLimit {
		r.failf("display list declares %d decoded entries; the budget is %d", r.total+n, decodedEntryLimit)
		return nil
	}
	r.total += n
	out := make([]T, 0, n)
	for range n {
		if r.err != nil {
			return nil
		}
		out = append(out, item(r))
	}
	return out
}

func (r *listReader) failf(format string, args ...any) {
	if r.err == nil {
		r.err = fail(FailureProtocol, format, args...)
	}
}

func (r *listReader) take(n int) []byte {
	if r.err != nil {
		return nil
	}
	if n < 0 || n > len(r.buf) {
		r.failf("display list ends %d bytes early", n-len(r.buf))
		return nil
	}
	out := r.buf[:n:n]
	r.buf = r.buf[n:]
	return out
}

func (r *listReader) byte() byte {
	b := r.take(1)
	if b == nil {
		return 0
	}
	return b[0]
}

func (r *listReader) float() float32 {
	b := r.take(4)
	if b == nil {
		return 0
	}
	return math.Float32frombits(binary.LittleEndian.Uint32(b))
}

func (r *listReader) uvarint() uint64 {
	if r.err != nil {
		return 0
	}
	v, n := binary.Uvarint(r.buf)
	if n <= 0 {
		r.failf("display list holds a malformed count")
		return 0
	}
	r.buf = r.buf[n:]
	return v
}

func (r *listReader) varint() int32 {
	if r.err != nil {
		return 0
	}
	v, n := binary.Varint(r.buf)
	if n <= 0 || v < math.MinInt32 || v > math.MaxInt32 {
		r.failf("display list holds a malformed index")
		return 0
	}
	r.buf = r.buf[n:]
	return int32(v)
}

func (r *listReader) count(minSize int) int {
	v := r.uvarint()
	if v > OperationLimit || int(v) > len(r.buf)/minSize {
		r.failf("display list declares %d entries in %d bytes", v, len(r.buf))
		return 0
	}
	return int(v)
}

func (r *listReader) string() string { return string(r.take(r.count(1))) }

func (r *listReader) point() Point { return Point{X: r.float(), Y: r.float()} }

func (r *listReader) color() Color {
	b := r.take(4)
	if b == nil {
		return Color{}
	}
	return Color{R: b[0], G: b[1], B: b[2], A: b[3]}
}

func (r *listReader) paint() Paint { return Paint{Color: r.color(), Gradient: r.varint()} }

func (r *listReader) affine() Affine {
	return Affine{A: r.float(), B: r.float(), C: r.float(), D: r.float(), E: r.float(), F: r.float()}
}

func (r *listReader) path() Path {
	verbs := r.take(r.count(1))
	p := Path{Verbs: make([]Verb, len(verbs))}
	for i, v := range verbs {
		p.Verbs[i] = Verb(v)
	}
	p.Points = readSlice(r, 8, (*listReader).point)
	return p
}

func (r *listReader) gradient() Gradient {
	g := Gradient{Start: r.point(), End: r.point()}
	g.Stops = readSlice(r, 8, func(r *listReader) Stop { return Stop{Offset: r.float(), Color: r.color()} })
	g.Spread = Spread(r.byte())
	g.Space = r.affine()
	return g
}

func (r *listReader) chunk() TextChunk {
	c := TextChunk{Origin: r.point(), Anchor: Anchor(r.byte()), Label: r.varint()}
	c.Runs = readSlice(r, 20, (*listReader).run)
	return c
}

func (r *listReader) run() TextRun {
	run := TextRun{Text: r.string(), Size: r.float()}
	weight := r.uvarint()
	if weight > math.MaxUint16 {
		r.failf("text weight %d is out of range", weight)
		return run
	}
	run.Weight = uint16(weight)
	run.Italic = r.byte() == 1
	run.Face = Face(r.byte())
	run.Baseline = Baseline(r.byte())
	run.Fill = r.paint()
	run.Shift = r.point()
	return run
}

func (r *listReader) tile() Tile {
	t := Tile{Bounds: Rect{Min: r.point(), Max: r.point()}}
	t.Width = int(min(r.uvarint(), TileSideLimit+1))
	t.Height = int(min(r.uvarint(), TileSideLimit+1))
	t.Pix = r.take(r.count(1))
	return t
}

func (r *listReader) op() Op {
	o := Op{Kind: OpKind(r.byte())}
	switch o.Kind {
	case OpTransform:
		o.Transform = r.affine()
	case OpOpacity:
		o.Alpha = r.float()
	case OpFill:
		o.Index = r.varint()
		o.Paint = r.paint()
	case OpClip, OpText, OpImage:
		o.Index = r.varint()
	case OpPop:
	default:
		r.failf("display operation %d is not registered", o.Kind)
	}
	return o
}
