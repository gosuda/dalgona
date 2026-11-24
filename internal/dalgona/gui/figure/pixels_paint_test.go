//go:build gui

package figure

import (
	"image"
	"testing"

	"gioui.org/layout"
	"gioui.org/op"
)

func admitPaintPreview(t *testing.T, ledger *PixelLedger) (*PixelCache, *ImagePaint, PreviewKey) {
	t.Helper()
	c := NewPixelCache(ledger)
	key := PreviewKey{Image: ImageDigest([]byte("paint-ledger")), Side: 256}
	if err := c.Admit(key, solidPreview(256, 7)); err != nil {
		t.Fatal(err)
	}
	d, ok := c.Paint(key)
	if !ok {
		t.Fatal("Paint of an admitted preview failed")
	}
	return c, d, key
}

func TestCacheOwnedPaintReportsItsBytes(t *testing.T) {
	ledger := NewPixelLedger(1 << 20)
	_, d, _ := admitPaintPreview(t, ledger)
	if used := ledger.Used(); used <= 256*256*4 {
		t.Fatalf("ledger used = %d after Paint, want the tile bytes plus the paint charge", used)
	}
	if !d.Live() {
		t.Fatal("fresh cache-owned paint is not live")
	}
	var gtx layout.Context
	gtx.Ops = new(op.Ops)
	if dims := d.Layout(gtx, image.Pt(128, 128), "preview"); dims.Size.X <= 0 {
		t.Fatalf("live paint laid out to %+v, want a thumbnail", dims.Size)
	}
	d.Release()
}

func TestCacheOwnedPaintEvictsWithLedger(t *testing.T) {
	ledger := NewPixelLedger(1 << 20)
	_, d, _ := admitPaintPreview(t, ledger)
	if err := ledger.Charge("junk", ledger.Limit(), func() {}); err != nil {
		t.Fatal(err)
	}
	if d.Live() {
		t.Fatal("paint stayed live after the ledger evicted its charge")
	}
	var gtx layout.Context
	gtx.Ops = new(op.Ops)
	if dims := d.Layout(gtx, image.Pt(128, 128), "preview"); dims.Size.X != 0 {
		t.Fatalf("evicted paint laid out to %+v, want nothing", dims.Size)
	}
	if got := ledger.Used(); got > ledger.Limit() {
		t.Fatalf("ledger used = %d, over the %d limit after eviction", got, ledger.Limit())
	}
	d.Release()
}

func TestCacheOwnedPaintRefusesMissingPreview(t *testing.T) {
	c := NewPixelCache(NewPixelLedger(1 << 20))
	if _, ok := c.Paint(PreviewKey{Side: 256}); ok {
		t.Fatal("Paint of a missing preview succeeded")
	}
}
