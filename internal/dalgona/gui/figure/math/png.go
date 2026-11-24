package math

import (
	"fmt"
	"image"
	"image/color"
	"image/png"
	"io"
	stdmath "math"

	"github.com/go-text/typesetting/font/opentype"
	"golang.org/x/image/vector"
)

const (
	MaxExportPixels = 32_000_000
	MaxExportBytes  = 256 << 20
	rasterTile      = 256
)

var errExportTooLarge = fmt.Errorf("formula PNG exceeds %d MiB; choose a smaller scale", MaxExportBytes>>20)

var inkAlpha = func() (colors [256]color.Color) {
	for i := range colors {
		colors[i] = color.NRGBA{A: uint8(i)}
	}
	return colors
}()

func (l *Layout) WritePNG(w io.Writer, scale float32) error {
	bounds, err := exportBounds(l.Width*scale, (l.Ascent+l.Descent)*scale)
	if err != nil {
		return err
	}
	return png.Encode(&limitedWriter{w: w, left: MaxExportBytes}, &raster{layout: l, scale: scale, bounds: bounds})
}

func exportBounds(width, height float32) (image.Rectangle, error) {
	columns, rows := stdmath.Ceil(float64(width)), stdmath.Ceil(float64(height))
	if !(columns >= 1 && rows >= 1 && columns*rows <= MaxExportPixels) {
		return image.Rectangle{}, fmt.Errorf("formula PNG needs %.0f by %.0f pixels; choose a scale that keeps it between 1 and %d pixels", columns, rows, MaxExportPixels)
	}
	return image.Rect(0, 0, int(columns), int(rows)), nil
}

type limitedWriter struct {
	w    io.Writer
	left int
}

func (l *limitedWriter) Write(p []byte) (int, error) {
	if len(p) > l.left {
		return 0, errExportTooLarge
	}
	l.left -= len(p)
	return l.w.Write(p)
}

type raster struct {
	layout *Layout
	scale  float32
	bounds image.Rectangle
	band   image.Alpha
	vector vector.Rasterizer
}

func (r *raster) ColorModel() color.Model { return color.NRGBAModel }

func (r *raster) Bounds() image.Rectangle { return r.bounds }

func (r *raster) Opaque() bool { return false }

func (r *raster) At(x, y int) color.Color {
	if !image.Pt(x, y).In(r.band.Rect) {
		r.paint(y - y%rasterTile)
	}
	return inkAlpha[r.band.Pix[r.band.PixOffset(x, y)]]
}

func (r *raster) paint(top int) {
	width, rows := r.bounds.Dx(), min(rasterTile, r.bounds.Dy()-top)
	if cap(r.band.Pix) < width*rows {
		r.band.Pix = make([]byte, width*rows)
	}
	r.band.Pix = r.band.Pix[:width*rows]
	clear(r.band.Pix)
	r.band.Stride = width
	r.band.Rect = image.Rect(0, top, width, top+rows)
	r.layout.outline(func(path []Segment) bool {
		r.fill(path)
		return true
	})
}

func (r *raster) fill(path []Segment) {
	area := r.pixelBounds(path).Intersect(r.band.Rect)
	for left := area.Min.X; left < area.Max.X; left += rasterTile {
		r.fillTile(path, image.Rect(left, area.Min.Y, min(left+rasterTile, area.Max.X), area.Max.Y))
	}
}

func (r *raster) pixelBounds(path []Segment) image.Rectangle {
	left, top := float32(stdmath.Inf(1)), float32(stdmath.Inf(1))
	right, bottom := -left, -top
	for _, segment := range path {
		for _, p := range segment.points() {
			left, right = min(left, p.X), max(right, p.X)
			top, bottom = min(top, p.Y), max(bottom, p.Y)
		}
	}
	return image.Rect(pixelFloor(left*r.scale), pixelFloor(top*r.scale), pixelCeil(right*r.scale), pixelCeil(bottom*r.scale))
}

func pixelFloor(v float32) int {
	return int(stdmath.Floor(clampPixel(v)))
}

func pixelCeil(v float32) int {
	return int(stdmath.Ceil(clampPixel(v)))
}

func clampPixel(v float32) float64 {
	return float64(min(max(v, -1), MaxExportPixels+1))
}

func (r *raster) fillTile(path []Segment, tile image.Rectangle) {
	r.vector.Reset(tile.Dx(), tile.Dy())
	shift := Point{X: float32(tile.Min.X), Y: float32(tile.Min.Y)}
	for i, segment := range path {
		if segment.Op == opentype.SegmentOpMoveTo && i > 0 {
			r.vector.ClosePath()
		}
		r.trace(segment, shift)
	}
	r.vector.ClosePath()
	r.vector.Draw(&r.band, tile, image.Opaque, image.Point{})
}

func (r *raster) trace(segment Segment, shift Point) {
	var at [3]Point
	for i, p := range segment.points() {
		at[i] = Point{X: p.X*r.scale - shift.X, Y: p.Y*r.scale - shift.Y}
	}
	switch segment.Op {
	case opentype.SegmentOpMoveTo:
		r.vector.MoveTo(at[0].X, at[0].Y)
	case opentype.SegmentOpLineTo:
		r.vector.LineTo(at[0].X, at[0].Y)
	case opentype.SegmentOpQuadTo:
		r.vector.QuadTo(at[0].X, at[0].Y, at[1].X, at[1].Y)
	case opentype.SegmentOpCubeTo:
		r.vector.CubeTo(at[0].X, at[0].Y, at[1].X, at[1].Y, at[2].X, at[2].Y)
	}
}
