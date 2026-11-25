//go:build gui

package figure

import (
	"image"
	"sync"

	"gioui.org/f32"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/op/paint"
)

type ImagePaint struct {
	cache *PixelCache
	key   PreviewKey
	size  image.Point
	src   image.Point
	mu    sync.Mutex
	live  bool
	tiles []Tile
	ops   []paint.ImageOp
}

func (c *PixelCache) Paint(key PreviewKey) (*ImagePaint, bool) {
	c.mu.Lock()
	shape, ok := c.previews[key]
	if !ok {
		c.mu.Unlock()
		return nil, false
	}
	tiles := make([]Tile, 0, len(c.tiles))
	total := 0
	for y := 0; y < shape.height; y += TileSideLimit {
		for x := 0; x < shape.width; x += TileSideLimit {
			t, ok := c.tiles[TileKey{Preview: key, X: x, Y: y}]
			if !ok {
				c.mu.Unlock()
				return nil, false
			}
			tiles = append(tiles, t)
			total += len(t.Pix)
		}
	}
	d := &ImagePaint{cache: c, key: key, size: image.Pt(shape.width, shape.height), src: image.Pt(shape.sourceWidth, shape.sourceHeight), live: true}
	c.mu.Unlock()
	d.retile(tiles)
	if err := c.ledger.Charge(d, total, d.invalidate); err != nil {
		d.invalidate()
		return nil, false
	}
	return d, true
}

func NewImagePaint(p *Preview) *ImagePaint {
	d := &ImagePaint{size: image.Pt(p.Width, p.Height), live: true}
	d.retile(p.Tiles)
	return d
}

func (d *ImagePaint) retile(tiles []Tile) {
	d.tiles = tiles
	d.ops = make([]paint.ImageOp, len(tiles))
	for i, t := range tiles {
		pix := &image.RGBA{Pix: t.Pix, Stride: t.Width * 4, Rect: image.Rect(0, 0, t.Width, t.Height)}
		d.ops[i] = paint.NewImageOp(pix)
	}
}

func (d *ImagePaint) invalidate() {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.live = false
	d.tiles = nil
	d.ops = nil
}

func (d *ImagePaint) Release() {
	if d.cache != nil {
		d.cache.ledger.Discharge(d)
	}
	d.invalidate()
}

func (d *ImagePaint) Live() bool {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.live
}

func (d *ImagePaint) Size() image.Point {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.size
}

func (d *ImagePaint) SourceSize() image.Point {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.src
}

func (d *ImagePaint) Layout(gtx layout.Context, limit image.Point, label string) layout.Dimensions {
	d.mu.Lock()
	defer d.mu.Unlock()
	if !d.live || limit.X <= 0 || limit.Y <= 0 {
		return layout.Dimensions{}
	}
	if d.cache != nil {
		d.cache.ledger.Touch(d)
	}
	scale := min(float32(limit.X)/float32(d.size.X), float32(limit.Y)/float32(d.size.Y), 1)
	size := image.Pt(int(float32(d.size.X)*scale+0.5), int(float32(d.size.Y)*scale+0.5))
	defer clip.Rect{Max: size}.Push(gtx.Ops).Pop()
	semantic.LabelOp(label).Add(gtx.Ops)
	for i, tile := range d.tiles {
		at := f32.Pt(tile.Bounds.Min.X*scale, tile.Bounds.Min.Y*scale)
		stack := op.Affine(f32.AffineId().Scale(f32.Point{}, f32.Pt(scale, scale)).Offset(at)).Push(gtx.Ops)
		tileArea := clip.Rect{Max: image.Pt(tile.Width, tile.Height)}.Push(gtx.Ops)
		d.ops[i].Add(gtx.Ops)
		paint.PaintOp{}.Add(gtx.Ops)
		tileArea.Pop()
		stack.Pop()
	}
	return layout.Dimensions{Size: size}
}
