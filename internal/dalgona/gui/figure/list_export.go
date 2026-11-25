package figure

import (
	"bufio"
	"encoding/base64"
	"encoding/xml"
	"errors"
	"fmt"
	"image"
	"image/color"
	"image/png"
	"io"
	"math"
	"strconv"

	"github.com/go-text/typesetting/font/opentype"

	figmath "github.com/gosuda/dalgona/internal/dalgona/gui/figure/math"
)

const (
	exportChunk = 32 << 10
	exportTile  = TileSideLimit
	exportBand  = 1 << 20
	hexDigits   = "0123456789abcdef"
)

var (
	errFigureTooLarge = fmt.Errorf("figure PNG exceeds %d MiB; choose a smaller scale", figmath.MaxExportBytes>>20)
	errNoTextFace     = errors.New("figure text needs a text face; pass the document text font to the export")
	spreadMethods     = [...]string{SpreadPad: "pad", SpreadReflect: "reflect", SpreadRepeat: "repeat"}
	pathCommands      = [...]byte{VerbMove: 'M', VerbLine: 'L', VerbQuad: 'Q', VerbCube: 'C', VerbClose: 'Z'}
	rectVerbs         = []Verb{VerbMove, VerbLine, VerbLine, VerbLine, VerbClose}
	clipInk           = rasterPaint(func(vec) rgba { return rgba{r: 1, g: 1, b: 1, a: 1} })
)

func checkExport(l *List, text figmath.TextFace) error {
	if err := l.Validate(); err != nil {
		return err
	}
	if text == nil && len(l.Chunks) > 0 {
		return errNoTextFace
	}
	return nil
}

func WriteSVG(w io.Writer, l *List, text figmath.TextFace) error {
	if err := checkExport(l, text); err != nil {
		return err
	}
	e := &svgExport{out: bufio.NewWriterSize(w, exportChunk), list: l, text: textPlacer{face: text, mono: text}}
	if err := e.document(); err != nil {
		return err
	}
	return e.out.Flush()
}

func WriteSVGWithFaces(w io.Writer, l *List, text, mono figmath.TextFace) error {
	if mono == nil {
		mono = text
	}
	if err := checkExport(l, text); err != nil {
		return err
	}
	e := &svgExport{out: bufio.NewWriterSize(w, exportChunk), list: l, text: textPlacer{face: text, mono: mono}}
	if err := e.document(); err != nil {
		return err
	}
	return e.out.Flush()
}

type svgExport struct {
	out   *bufio.Writer
	b     []byte
	list  *List
	text  textPlacer
	clips int
}

func (e *svgExport) document() error {
	if err := e.header(); err != nil {
		return err
	}
	if err := e.gradients(); err != nil {
		return err
	}
	for _, o := range e.list.Ops {
		if err := e.op(o); err != nil {
			return err
		}
	}
	e.b = append(e.b, "</svg>\n"...)
	return e.emit()
}

func (e *svgExport) emit() error {
	_, err := e.out.Write(e.b)
	e.b = e.b[:0]
	return err
}

func (e *svgExport) spill() error {
	if len(e.b) < exportChunk {
		return nil
	}
	return e.emit()
}

func (e *svgExport) header() error {
	l := e.list
	e.b = append(e.b, `<?xml version="1.0" encoding="UTF-8"?>`+"\n"+`<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="`...)
	e.b = svgNumber(e.b, l.Width)
	e.b = append(e.b, `" height="`...)
	e.b = svgNumber(e.b, l.Height)
	e.b = append(e.b, `" viewBox="0 0 `...)
	e.b = svgNumber(e.b, l.Width)
	e.b = append(e.b, ' ')
	e.b = svgNumber(e.b, l.Height)
	e.b = append(e.b, "\">\n"...)
	if l.Title == "" {
		return e.emit()
	}
	e.b = append(e.b, "<title>"...)
	if err := e.emit(); err != nil {
		return err
	}
	if err := xml.EscapeText(e.out, []byte(l.Title)); err != nil {
		return err
	}
	e.b = append(e.b, "</title>\n"...)
	return e.emit()
}

func (e *svgExport) gradients() error {
	if len(e.list.Gradients) == 0 {
		return nil
	}
	e.b = append(e.b, "<defs>\n"...)
	for i, g := range e.list.Gradients {
		if err := e.gradient(i, g); err != nil {
			return err
		}
	}
	e.b = append(e.b, "</defs>\n"...)
	return e.emit()
}

func (e *svgExport) gradient(i int, g Gradient) error {
	e.b = append(e.b, `<linearGradient id="g`...)
	e.b = strconv.AppendInt(e.b, int64(i), 10)
	e.b = append(e.b, `" gradientUnits="userSpaceOnUse" x1="`...)
	e.b = svgNumber(e.b, g.Start.X)
	e.b = append(e.b, `" y1="`...)
	e.b = svgNumber(e.b, g.Start.Y)
	e.b = append(e.b, `" x2="`...)
	e.b = svgNumber(e.b, g.End.X)
	e.b = append(e.b, `" y2="`...)
	e.b = svgNumber(e.b, g.End.Y)
	e.b = append(e.b, `" spreadMethod="`...)
	e.b = append(e.b, spreadMethods[g.Spread]...)
	e.b = append(e.b, `" gradientTransform="`...)
	e.b = svgMatrix(e.b, g.Space)
	e.b = append(e.b, "\">\n"...)
	for _, s := range g.Stops {
		e.b = append(e.b, `<stop offset="`...)
		e.b = svgNumber(e.b, s.Offset)
		e.b = append(e.b, '"')
		e.b = svgColor(e.b, " stop-color", " stop-opacity", s.Color)
		e.b = append(e.b, "/>\n"...)
		if err := e.spill(); err != nil {
			return err
		}
	}
	e.b = append(e.b, "</linearGradient>\n"...)
	return e.spill()
}

func (e *svgExport) op(o Op) error {
	switch o.Kind {
	case OpTransform:
		e.b = append(e.b, `<g transform="`...)
		e.b = svgMatrix(e.b, o.Transform)
		e.b = append(e.b, "\">\n"...)
	case OpClip:
		return e.clip(e.list.Paths[o.Index])
	case OpOpacity:
		e.b = append(e.b, `<g opacity="`...)
		e.b = svgNumber(e.b, o.Alpha)
		e.b = append(e.b, "\">\n"...)
	case OpPop:
		e.b = append(e.b, "</g>\n"...)
	case OpFill:
		return e.filled(e.list.Paths[o.Index], o.Paint)
	case OpText:
		return e.text.place(e.list.Chunks[o.Index], func(fill Color, p Path) error { return e.filled(p, SolidPaint(fill)) })
	case OpImage:
		return e.image(e.list.Tiles[o.Index])
	}
	return e.emit()
}

func (e *svgExport) clip(p Path) error {
	e.clips++
	e.b = append(e.b, `<clipPath id="c`...)
	e.b = strconv.AppendInt(e.b, int64(e.clips), 10)
	e.b = append(e.b, `"><path d="`...)
	if err := e.pathData(p); err != nil {
		return err
	}
	e.b = append(e.b, `"/></clipPath>`+"\n"+`<g clip-path="url(#c`...)
	e.b = strconv.AppendInt(e.b, int64(e.clips), 10)
	e.b = append(e.b, ")\">\n"...)
	return e.emit()
}

func (e *svgExport) filled(p Path, paint Paint) error {
	if len(p.Verbs) == 0 {
		return nil
	}
	e.b = append(e.b, `<path d="`...)
	if err := e.pathData(p); err != nil {
		return err
	}
	e.b = append(e.b, '"')
	e.b = svgPaint(e.b, paint)
	e.b = append(e.b, "/>\n"...)
	return e.emit()
}

func (e *svgExport) pathData(p Path) error {
	k := 0
	for i, v := range p.Verbs {
		if i > 0 {
			e.b = append(e.b, ' ')
		}
		e.b = append(e.b, pathCommands[v])
		n := verbPoints[v]
		for _, pt := range p.Points[k : k+n] {
			e.b = append(e.b, ' ')
			e.b = svgNumber(e.b, pt.X)
			e.b = append(e.b, ' ')
			e.b = svgNumber(e.b, pt.Y)
		}
		k += n
		if err := e.spill(); err != nil {
			return err
		}
	}
	return nil
}

func (e *svgExport) image(t Tile) error {
	e.b = append(e.b, `<image width="`...)
	e.b = strconv.AppendInt(e.b, int64(t.Width), 10)
	e.b = append(e.b, `" height="`...)
	e.b = strconv.AppendInt(e.b, int64(t.Height), 10)
	e.b = append(e.b, `" preserveAspectRatio="none" transform="`...)
	e.b = svgMatrix(e.b, tileAffine(t))
	e.b = append(e.b, `" xlink:href="data:image/png;base64,`...)
	if err := e.emit(); err != nil {
		return err
	}
	data := base64.NewEncoder(base64.StdEncoding, e.out)
	pixels := &image.RGBA{Pix: t.Pix, Stride: 4 * t.Width, Rect: image.Rect(0, 0, t.Width, t.Height)}
	if err := png.Encode(data, pixels); err != nil {
		return err
	}
	if err := data.Close(); err != nil {
		return err
	}
	e.b = append(e.b, "\"/>\n"...)
	return e.emit()
}

func tileAffine(t Tile) Affine {
	return Affine{A: t.Bounds.Width() / float32(t.Width), D: t.Bounds.Height() / float32(t.Height), E: t.Bounds.Min.X, F: t.Bounds.Min.Y}
}

func svgNumber(b []byte, v float32) []byte {
	return strconv.AppendFloat(b, float64(v), 'g', -1, 32)
}

func svgMatrix(b []byte, m Affine) []byte {
	b = append(b, "matrix("...)
	for i, v := range [...]float32{m.A, m.B, m.C, m.D, m.E, m.F} {
		if i > 0 {
			b = append(b, ' ')
		}
		b = svgNumber(b, v)
	}
	return append(b, ')')
}

func svgColor(b []byte, name, opacity string, c Color) []byte {
	b = append(b, name...)
	b = append(b, `="#`...)
	for _, v := range [...]uint8{c.R, c.G, c.B} {
		b = append(b, hexDigits[v>>4], hexDigits[v&15])
	}
	b = append(b, '"')
	if c.A == 255 {
		return b
	}
	b = append(b, opacity...)
	b = append(b, `="`...)
	b = strconv.AppendFloat(b, float64(c.A)/255, 'g', -1, 64)
	return append(b, '"')
}

func svgPaint(b []byte, p Paint) []byte {
	if p.Gradient < 0 {
		return svgColor(b, " fill", " fill-opacity", p.Color)
	}
	b = append(b, ` fill="url(#g`...)
	b = strconv.AppendInt(b, int64(p.Gradient), 10)
	return append(b, `)"`...)
}

type textPlacer struct {
	face    figmath.TextFace
	mono    figmath.TextFace
	metrics []figmath.TextMetrics
	outline []figmath.Segment
	path    Path
}

func (p *textPlacer) faceFor(r TextRun) figmath.TextFace {
	if r.Face == FaceMono && p.mono != nil {
		return p.mono
	}
	return p.face
}

func (p *textPlacer) place(c TextChunk, visit func(Color, Path) error) error {
	width := p.measure(c.Runs)
	pen := Point{X: c.Origin.X - anchorShift(c.Anchor, width), Y: c.Origin.Y}
	for i, r := range c.Runs {
		m := p.metrics[i]
		pen = Point{X: pen.X + r.Shift.X, Y: pen.Y + r.Shift.Y}
		p.trace(r, Point{X: pen.X, Y: pen.Y - runDrop(r.Baseline, m) + m.Ascent})
		if err := visit(r.Fill.Color, p.path); err != nil {
			return err
		}
		pen.X += m.Width
	}
	return nil
}

func (p *textPlacer) measure(runs []TextRun) float32 {
	p.metrics = p.metrics[:0]
	width := float32(0)
	for _, r := range runs {
		m := p.faceFor(r).MeasureText(r.Text, r.Size)
		p.metrics = append(p.metrics, m)
		width += r.Shift.X + m.Width
	}
	return width
}

func (p *textPlacer) trace(r TextRun, origin Point) {
	p.outline = p.faceFor(r).AppendOutline(p.outline[:0], r.Text, r.Size)
	p.path.Verbs, p.path.Points = p.path.Verbs[:0], p.path.Points[:0]
	for i, s := range p.outline {
		v := segmentVerb(s.Op)
		if v == VerbMove && i > 0 {
			p.path.Verbs = append(p.path.Verbs, VerbClose)
		}
		p.path.Verbs = append(p.path.Verbs, v)
		for _, q := range s.Points[:verbPoints[v]] {
			p.path.Points = append(p.path.Points, Point{X: origin.X + q.X, Y: origin.Y + q.Y})
		}
	}
	if len(p.outline) > 0 {
		p.path.Verbs = append(p.path.Verbs, VerbClose)
	}
}

func segmentVerb(op opentype.SegmentOp) Verb {
	switch op {
	case opentype.SegmentOpLineTo:
		return VerbLine
	case opentype.SegmentOpQuadTo:
		return VerbQuad
	case opentype.SegmentOpCubeTo:
		return VerbCube
	case opentype.SegmentOpMoveTo:
	}
	return VerbMove
}

func anchorShift(a Anchor, width float32) float32 {
	switch a {
	case AnchorMiddle:
		return width / 2
	case AnchorEnd:
		return width
	case AnchorStart:
	}
	return 0
}

func runDrop(b Baseline, m figmath.TextMetrics) float32 {
	height := m.Ascent + m.Descent
	switch b {
	case BaselineMiddle, BaselineCentral, BaselineMathematical:
		return height / 2
	case BaselineHanging, BaselineTextTop:
		return 0
	case BaselineTextBottom, BaselineIdeographic:
		return height
	case BaselineAlphabetic:
	}
	return m.Ascent
}

func WritePNG(w io.Writer, l *List, text figmath.TextFace, scale float32) error {
	return WritePNGWithFaces(w, l, text, nil, scale)
}

func WritePNGWithFaces(w io.Writer, l *List, text, mono figmath.TextFace, scale float32) error {
	if mono == nil {
		mono = text
	}
	img, err := newListImage(l, text, mono, scale)
	if err != nil {
		return err
	}
	return png.Encode(&exportWriter{w: w, left: figmath.MaxExportBytes}, img)
}

func pngBounds(width, height float32) (image.Rectangle, error) {
	columns, rows := math.Ceil(float64(width)), math.Ceil(float64(height))
	if columns >= 1 && rows >= 1 && columns*rows <= figmath.MaxExportPixels {
		return image.Rect(0, 0, int(columns), int(rows)), nil
	}
	return image.Rectangle{}, fmt.Errorf("figure PNG needs %.0f by %.0f pixels; choose a scale that keeps it between 1 and %d pixels", columns, rows, figmath.MaxExportPixels)
}

type exportWriter struct {
	w    io.Writer
	left int
}

func (x *exportWriter) Write(p []byte) (int, error) {
	if len(p) > x.left {
		return 0, errFigureTooLarge
	}
	x.left -= len(p)
	return x.w.Write(p)
}

type listImage struct {
	list     *List
	text     textPlacer
	device   matrix
	viewport Path
	bounds   image.Rectangle
	rows     int
	top      int
	filled   int
	band     []color.RGBA
	boxes    []Rect
	tile     compositor
}

func newListImage(l *List, text, mono figmath.TextFace, scale float32) (*listImage, error) {
	if err := checkExport(l, text); err != nil {
		return nil, err
	}
	bounds, err := pngBounds(l.Width*scale, l.Height*scale)
	if err != nil {
		return nil, err
	}
	return &listImage{
		list:     l,
		text:     textPlacer{face: text, mono: mono},
		device:   matrix{a: float64(scale), d: float64(scale)},
		viewport: Path{Verbs: rectVerbs, Points: []Point{{0, 0}, {l.Width, 0}, {l.Width, l.Height}, {0, l.Height}}},
		bounds:   bounds,
		rows:     min(exportTile, max(1, exportBand/bounds.Dx())),
	}, nil
}

func (r *listImage) ColorModel() color.Model { return color.RGBAModel }

func (r *listImage) Bounds() image.Rectangle { return r.bounds }

func (r *listImage) Opaque() bool { return false }

func (r *listImage) At(x, y int) color.Color {
	if !image.Pt(x, y).In(r.bounds) {
		return color.RGBA{}
	}
	if y < r.top || y >= r.top+r.filled {
		r.paint(y - y%r.rows)
	}
	return &r.band[(y-r.top)*r.bounds.Dx()+x]
}

func (r *listImage) paint(top int) {
	width := r.bounds.Dx()
	rows := min(r.rows, r.bounds.Dy()-top)
	if cap(r.band) < width*rows {
		r.band = make([]color.RGBA, width*rows)
	}
	r.band = r.band[:width*rows]
	r.top, r.filled = top, rows
	if r.boxes == nil {
		r.boxes = r.inkBoxes()
	}
	for left := 0; left < width; left += exportTile {
		r.paintTile(image.Rect(left, top, min(left+exportTile, width), top+rows))
	}
}

func (r *listImage) inkBoxes() []Rect {
	boxes := make([]Rect, len(r.list.Chunks))
	for i, c := range r.list.Chunks {
		box := Rect{Min: Point{math.MaxFloat32, math.MaxFloat32}, Max: Point{-math.MaxFloat32, -math.MaxFloat32}}
		_ = r.text.place(c, func(_ Color, p Path) error {
			for _, pt := range p.Points {
				box.Min = Point{X: min(box.Min.X, pt.X), Y: min(box.Min.Y, pt.Y)}
				box.Max = Point{X: max(box.Max.X, pt.X), Y: max(box.Max.Y, pt.Y)}
			}
			return nil
		})
		boxes[i] = box
	}
	return boxes
}

func (r *listImage) paintTile(area image.Rectangle) {
	c := &r.tile
	c.begin(area, r.device)
	c.pushClip(r.viewport)
	for _, o := range r.list.Ops {
		r.op(o)
	}
	c.pop()
	c.finish(r.band[area.Min.X:], r.bounds.Dx())
}

func (r *listImage) op(o Op) {
	c := &r.tile
	switch o.Kind {
	case OpTransform:
		c.pushTransform(matrixOf(o.Transform))
	case OpClip:
		c.pushClip(r.list.Paths[o.Index])
	case OpOpacity:
		c.pushOpacity(o.Alpha)
	case OpPop:
		c.pop()
	case OpFill:
		r.fill(o)
	case OpText:
		r.textChunk(o.Index)
	case OpImage:
		r.image(r.list.Tiles[o.Index])
	}
}

func (r *listImage) fill(o Op) {
	c := &r.tile
	f := c.top()
	p := r.list.Paths[o.Index]
	if !c.visible(f, p.Points, f.ctm) {
		return
	}
	if ink, ok := r.ink(o.Paint, f.ctm); ok {
		c.paint(p, f.ctm, ink)
	}
}

func (r *listImage) ink(p Paint, ctm matrix) (rasterPaint, bool) {
	if p.Gradient < 0 {
		return solidInk(p.Color), true
	}
	g := r.list.Gradients[p.Gradient]
	spread := g.Spread
	if g.Start == g.End {
		spread = SpreadPad
	}
	def := &gradientDef{
		x1: float64(g.Start.X), y1: float64(g.Start.Y), x2: float64(g.End.X), y2: float64(g.End.Y),
		spread: spread, stops: stopDefs(g.Stops), toUser: ctm.mul(matrixOf(g.Space)),
	}
	return def.rasterPaint()
}

func (r *listImage) textChunk(index int32) {
	c := &r.tile
	f := c.top()
	box := r.boxes[index]
	corners := [...]Point{box.Min, box.Max, {X: box.Min.X, Y: box.Max.Y}, {X: box.Max.X, Y: box.Min.Y}}
	if box.Min.X > box.Max.X || !c.visible(f, corners[:], f.ctm) {
		return
	}
	ctm := f.ctm
	_ = r.text.place(r.list.Chunks[index], func(fill Color, p Path) error {
		c.paint(p, ctm, solidInk(fill))
		return nil
	})
}

func (r *listImage) image(t Tile) {
	c := &r.tile
	f := c.top()
	m := f.ctm.mul(matrixOf(tileAffine(t)))
	w, h := float32(t.Width), float32(t.Height)
	frame := Path{Verbs: rectVerbs, Points: []Point{{0, 0}, {w, 0}, {w, h}, {0, h}}}
	inverse, ok := m.invert()
	if !ok || !c.visible(f, frame.Points, m) {
		return
	}
	c.paint(frame, m, func(p vec) rgba { return sampleTile(t, inverse.vec(p)) })
}

func solidInk(c Color) rasterPaint {
	ink := exportColor(c)
	return func(vec) rgba { return ink }
}

func exportColor(c Color) rgba {
	return rgba{r: float64(c.R) / 255, g: float64(c.G) / 255, b: float64(c.B) / 255, a: float64(c.A) / 255}
}

func stopDefs(stops []Stop) []stopDef {
	out := make([]stopDef, len(stops))
	for i, s := range stops {
		out[i] = stopDef{offset: float64(s.Offset), color: exportColor(s.Color)}
	}
	return out
}

func sampleTile(t Tile, p vec) rgba {
	x0, x1, fx := texel(p.x, t.Width)
	y0, y1, fy := texel(p.y, t.Height)
	top := mixTexel(texelAt(t, x0, y0), texelAt(t, x1, y0), fx)
	bottom := mixTexel(texelAt(t, x0, y1), texelAt(t, x1, y1), fx)
	c := mixTexel(top, bottom, fy)
	if c.a <= 0 {
		return rgba{}
	}
	return rgba{r: c.r / c.a, g: c.g / c.a, b: c.b / c.a, a: c.a}
}

func texel(v float64, n int) (int, int, float64) {
	v = math.Min(math.Max(v-0.5, 0), float64(n-1))
	i := int(v)
	return i, min(i+1, n-1), v - float64(i)
}

func texelAt(t Tile, x, y int) rgba {
	p := (*[4]byte)(t.Pix[(y*t.Width+x)*4:])
	return rgba{r: float64(p[0]) / 255, g: float64(p[1]) / 255, b: float64(p[2]) / 255, a: float64(p[3]) / 255}
}

func mixTexel(a, b rgba, f float64) rgba {
	return rgba{r: a.r + (b.r-a.r)*f, g: a.g + (b.g-a.g)*f, b: a.b + (b.b-a.b)*f, a: a.a + (b.a-a.a)*f}
}

type exportFrame struct {
	ctm      matrix
	mask     []float32
	layer    []float32
	alpha    float32
	ownMask  bool
	ownLayer bool
	hidden   bool
}

type compositor struct {
	area   image.Rectangle
	frames []exportFrame
	masks  [][]float32
	layers [][]float32
	poly   [2][]vec
}

func (c *compositor) begin(area image.Rectangle, device matrix) {
	c.area = area
	layer := takeBuffer(&c.layers, 4*area.Dx()*area.Dy())
	c.frames = append(c.frames[:0], exportFrame{ctm: device, layer: layer, ownLayer: true})
}

func (c *compositor) top() *exportFrame { return &c.frames[len(c.frames)-1] }

func (c *compositor) pushTransform(m matrix) {
	f := *c.top()
	f.ctm, f.ownMask, f.ownLayer = f.ctm.mul(m), false, false
	c.frames = append(c.frames, f)
}

func (c *compositor) pushOpacity(alpha float32) {
	f := *c.top()
	f.alpha, f.ownMask, f.ownLayer = alpha, false, !f.hidden
	if f.ownLayer {
		f.layer = takeBuffer(&c.layers, len(f.layer))
	}
	c.frames = append(c.frames, f)
}

func (c *compositor) pushClip(p Path) {
	f := *c.top()
	f.ownMask, f.ownLayer = false, false
	if c.visible(&f, p.Points, f.ctm) {
		f.mask, f.ownMask = c.clipMask(p, f.ctm, f.mask)
	}
	f.hidden = !f.ownMask
	c.frames = append(c.frames, f)
}

func (c *compositor) pop() {
	n := len(c.frames)
	f := c.frames[n-1]
	c.frames = c.frames[:n-1]
	if f.ownMask {
		c.masks = append(c.masks, f.mask)
	}
	if f.ownLayer {
		mergeLayer(c.frames[n-2].layer, f.layer, f.alpha)
		c.layers = append(c.layers, f.layer)
	}
}

func (c *compositor) finish(band []color.RGBA, stride int) {
	base := c.frames[0].layer
	width := c.area.Dx()
	for y := range c.area.Dy() {
		row := band[y*stride : y*stride+width]
		for x := range row {
			row[x] = rgbaPixel((*[4]float32)(base[4*(y*width+x):]))
		}
	}
	c.layers = append(c.layers, base)
	c.frames = c.frames[:0]
}

func (c *compositor) visible(f *exportFrame, points []Point, m matrix) bool {
	if f.hidden {
		return false
	}
	lo, hi := vec{math.Inf(1), math.Inf(1)}, vec{math.Inf(-1), math.Inf(-1)}
	for _, p := range points {
		v := m.vec(vec{float64(p.X), float64(p.Y)})
		lo = vec{math.Min(lo.x, v.x), math.Min(lo.y, v.y)}
		hi = vec{math.Max(hi.x, v.x), math.Max(hi.y, v.y)}
	}
	return lo.x < float64(c.area.Max.X) && hi.x > float64(c.area.Min.X) && lo.y < float64(c.area.Max.Y) && hi.y > float64(c.area.Min.Y)
}

func (c *compositor) clipMask(p Path, m matrix, parent []float32) ([]float32, bool) {
	tiles := c.coverage(p, m, clipInk)
	if len(tiles) == 0 {
		return nil, false
	}
	mask := takeBuffer(&c.masks, c.area.Dx()*c.area.Dy())
	for _, t := range tiles {
		c.cover(mask, t)
	}
	if parent != nil {
		for i := range mask {
			mask[i] *= parent[i]
		}
	}
	return mask, true
}

func (c *compositor) cover(mask []float32, t Tile) {
	start, width := c.origin(t), c.area.Dx()
	for y := range t.Height {
		row := mask[start+y*width : start+y*width+t.Width]
		for x := range row {
			row[x] = float32(t.Pix[(y*t.Width+x)*4+3]) / 255
		}
	}
}

func (c *compositor) paint(p Path, m matrix, ink rasterPaint) {
	f := c.top()
	if !c.visible(f, p.Points, m) {
		return
	}
	for _, t := range c.coverage(p, m, ink) {
		c.composite(f, t)
	}
}

func (c *compositor) composite(f *exportFrame, t Tile) {
	start, width := c.origin(t), c.area.Dx()
	for y := range t.Height {
		for x := range t.Width {
			s, i := (y*t.Width+x)*4, start+y*width+x
			if t.Pix[s+3] == 0 {
				continue
			}
			cover := float32(1)
			if f.mask != nil {
				cover = f.mask[i]
			}
			blendOver((*[4]float32)(f.layer[4*i:]), (*[4]byte)(t.Pix[s:]), cover)
		}
	}
}

func (c *compositor) origin(t Tile) int {
	return (int(t.Bounds.Min.Y)-c.area.Min.Y)*c.area.Dx() + int(t.Bounds.Min.X) - c.area.Min.X
}

func (c *compositor) coverage(p Path, m matrix, ink rasterPaint) []Tile {
	geom := c.clipToArea(shapeOf(p).transformed(m))
	if geom == nil {
		return nil
	}
	r, ok := newRaster(geom, ruleNonZero, 1)
	if !ok {
		return nil
	}
	return r.render(ink)
}

func (c *compositor) clipToArea(device *shape) *shape {
	out := &shape{}
	lo, hi := vec{math.Inf(1), math.Inf(1)}, vec{math.Inf(-1), math.Inf(-1)}
	for _, line := range device.flatten(deviceTolerance) {
		poly := c.clipPolygon(line.points)
		if len(poly) < 3 {
			continue
		}
		out.moveTo(poly[0])
		for _, v := range poly[1:] {
			out.lineTo(v)
		}
		out.close()
		lo, hi = growVec(lo, hi, poly)
	}
	if out.empty() {
		return nil
	}
	out.moveTo(vec{math.Floor(lo.x), math.Floor(lo.y)})
	out.close()
	out.moveTo(vec{math.Ceil(hi.x), math.Ceil(hi.y)})
	out.close()
	return out
}

func growVec(lo, hi vec, points []vec) (vec, vec) {
	for _, v := range points {
		lo = vec{math.Min(lo.x, v.x), math.Min(lo.y, v.y)}
		hi = vec{math.Max(hi.x, v.x), math.Max(hi.y, v.y)}
	}
	return lo, hi
}

func (c *compositor) clipPolygon(points []vec) []vec {
	lo := vec{float64(c.area.Min.X), float64(c.area.Min.Y)}
	hi := vec{float64(c.area.Max.X), float64(c.area.Max.Y)}
	planes := [...]halfPlane{{bound: lo.x, sign: 1}, {bound: hi.x, sign: -1}, {onY: true, bound: lo.y, sign: 1}, {onY: true, bound: hi.y, sign: -1}}
	for i, h := range planes {
		c.poly[i%2] = h.clip(points, c.poly[i%2][:0])
		points = c.poly[i%2]
	}
	return points
}

type halfPlane struct {
	onY   bool
	bound float64
	sign  float64
}

func (h halfPlane) distance(v vec) float64 {
	if h.onY {
		return (v.y - h.bound) * h.sign
	}
	return (v.x - h.bound) * h.sign
}

func (h halfPlane) clip(in, out []vec) []vec {
	for i, cur := range in {
		prev := in[(i+len(in)-1)%len(in)]
		dc, dp := h.distance(cur), h.distance(prev)
		if (dc >= 0) != (dp >= 0) {
			out = append(out, h.cross(prev, cur, dp/(dp-dc)))
		}
		if dc >= 0 {
			out = append(out, cur)
		}
	}
	return out
}

func (h halfPlane) cross(a, b vec, t float64) vec {
	v := lerp(a, b, t)
	if h.onY {
		v.y = h.bound
	} else {
		v.x = h.bound
	}
	return v
}

func takeBuffer(pool *[][]float32, n int) []float32 {
	var buf []float32
	if k := len(*pool); k > 0 {
		buf, *pool = (*pool)[k-1], (*pool)[:k-1]
	}
	if cap(buf) < n {
		return make([]float32, n)
	}
	buf = buf[:n]
	clear(buf)
	return buf
}

func blendOver(dst *[4]float32, src *[4]byte, cover float32) {
	k := cover / 255
	keep := 1 - float32(src[3])*k
	for i := range dst {
		dst[i] = float32(src[i])*k + dst[i]*keep
	}
}

func mergeLayer(dst, src []float32, alpha float32) {
	for i := range len(src) / 4 {
		s, d := (*[4]float32)(src[4*i:]), (*[4]float32)(dst[4*i:])
		keep := 1 - s[3]*alpha
		for k := range d {
			d[k] = s[k]*alpha + d[k]*keep
		}
	}
}

func rgbaPixel(p *[4]float32) color.RGBA {
	a := quantize(p[3])
	return color.RGBA{R: min(quantize(p[0]), a), G: min(quantize(p[1]), a), B: min(quantize(p[2]), a), A: a}
}

func quantize(v float32) uint8 { return uint8(min(max(v, 0), 1)*255 + 0.5) }
