package figure

import (
	"bytes"
	"image"
	"testing"
	"time"
)

func solidPreview(side int, shade byte) *Preview {
	p := &Preview{Width: side, Height: side, SourceWidth: side * 2, SourceHeight: side * 2}
	for y := 0; y < side; y += TileSideLimit {
		for x := 0; x < side; x += TileSideLimit {
			w, h := min(TileSideLimit, side-x), min(TileSideLimit, side-y)
			pix := make([]byte, w*h*4)
			for i := range pix {
				pix[i] = shade
			}
			bounds := Rect{Min: Point{float32(x), float32(y)}, Max: Point{float32(x + w), float32(y + h)}}
			p.Tiles = append(p.Tiles, Tile{Bounds: bounds, Width: w, Height: h, Pix: pix})
		}
	}
	return p
}

func previewKey(shade byte, side int) PreviewKey {
	return PreviewKey{Image: ImageDigest([]byte{shade}), Side: side}
}

func admitSolid(t *testing.T, c *PixelCache, shade byte, side int) PreviewKey {
	t.Helper()
	key := previewKey(shade, side)
	if err := c.Admit(key, solidPreview(side, shade)); err != nil {
		t.Fatal(err)
	}
	return key
}

func TestPixelCacheEvictsLeastRecentlyUsedTiles(t *testing.T) {
	tileBytes := TileSideLimit * TileSideLimit * 4
	c := NewPixelCache(NewPixelLedger(3 * tileBytes))
	first := admitSolid(t, c, 1, 256)
	second := admitSolid(t, c, 2, 256)
	if _, ok := c.Preview(first); !ok {
		t.Fatal("first preview missing before the cache is full")
	}
	third := admitSolid(t, c, 3, 256)
	fourth := admitSolid(t, c, 4, 256)
	if _, ok := c.Preview(second); ok {
		t.Fatal("the least recently used preview survived admission beyond the budget")
	}
	for shade, k := range map[byte]PreviewKey{1: first, 3: third, 4: fourth} {
		p, ok := c.Preview(k)
		if !ok || len(p.Tiles) != 1 || p.Tiles[0].Pix[0] != shade {
			t.Fatalf("preview %v = %+v, %v; want its own pixels", k, p, ok)
		}
	}
	if c.Used() != 3*tileBytes {
		t.Fatalf("cache holds %d bytes, want %d", c.Used(), 3*tileBytes)
	}
}

func TestPixelCacheKeysTilesByDigestAndPreviewSize(t *testing.T) {
	c := NewPixelCache(NewPixelLedger(PixelCacheLimit))
	key := admitSolid(t, c, 9, 600)
	if _, ok := c.Preview(previewKey(9, 300)); ok {
		t.Fatal("a preview of another size answered from the cache")
	}
	tile, ok := c.Tile(TileKey{Preview: key, X: 512, Y: 256})
	if !ok || tile.Width != 600-512 || tile.Height != 256 || tile.Bounds.Min != (Point{512, 256}) {
		t.Fatalf("tile at (512, 256) = %+v, %v; want the 88x256 edge tile", tile, ok)
	}
	if _, ok := c.Tile(TileKey{Preview: key, X: 100, Y: 0}); ok {
		t.Fatal("an unaligned tile origin answered from the cache")
	}
	c.releasePreview(key)
	if _, ok := c.Preview(key); ok || c.Used() != 0 {
		t.Fatalf("released image still cached; %d bytes held", c.Used())
	}
}

func TestPixelCacheKeepsOtherSizesWhenReleasingPreview(t *testing.T) {
	cache := NewPixelCache(NewPixelLedger(PixelCacheLimit))
	small := admitSolid(t, cache, 9, 32)
	large := admitSolid(t, cache, 9, 64)
	if !cache.HasPreview(small) || !cache.HasPreview(large) {
		t.Fatal("admission evicted another size of the same image")
	}
	cache.releasePreview(small)
	if cache.HasPreview(small) || !cache.HasPreview(large) || cache.Used() != 64*64*4 {
		t.Fatalf("releasing a thumbnail did not leave only the larger preview; cache holds %d bytes", cache.Used())
	}
}

func TestPixelCacheRefusesPreviewsLargerThanItsBudget(t *testing.T) {
	tileBytes := TileSideLimit * TileSideLimit * 4
	c := NewPixelCache(NewPixelLedger(tileBytes))
	if err := c.Admit(previewKey(1, 512), solidPreview(512, 1)); err == nil {
		t.Fatal("a four-tile preview entered a one-tile cache")
	}
	if c.Used() != 0 {
		t.Fatalf("refused preview left %d bytes behind", c.Used())
	}
	damaged := solidPreview(256, 1)
	damaged.Tiles[0].Pix = damaged.Tiles[0].Pix[:10]
	if err := c.Admit(previewKey(1, 256), damaged); err == nil {
		t.Fatal("a tile with missing pixels entered the cache")
	}
}

func TestPreviewKeyMatchesTheChildPreviewSide(t *testing.T) {
	r := ImageRequest{Format: ImagePNG, Data: []byte("png bytes"), Scale: 2, Viewport: [2]float64{300, 120}}
	key := PreviewKeyOf(r)
	if key.Side != 600 || key.Image != ImageDigest(r.Data) {
		t.Fatalf("preview key = %+v, want side 600 and the data digest", key)
	}
	r.Viewport = [2]float64{1e9, 1}
	if PreviewKeyOf(r).Side != previewSideLimit {
		t.Fatal("preview side is not bounded by the preview limit")
	}
}

func TestPixelLedgerSharesOneBudgetAcrossCaches(t *testing.T) {
	tileBytes := TileSideLimit * TileSideLimit * 4
	ledger := NewPixelLedger(2 * tileBytes)
	figures, thumbnails := NewPixelCache(ledger), NewPixelCache(ledger)
	first := admitSolid(t, figures, 1, 256)
	released := false
	if err := ledger.Charge("glyph tile", tileBytes, func() { released = true }); err != nil {
		t.Fatal(err)
	}
	admitSolid(t, thumbnails, 2, 256)
	if _, ok := figures.Preview(first); ok || figures.Used() != 0 {
		t.Fatalf("the oldest charge from another cache survived; figures hold %d bytes", figures.Used())
	}
	if released || ledger.Used() != 2*tileBytes {
		t.Fatalf("ledger holds %d bytes and released the glyph tile = %v; want %d bytes and the glyph tile kept", ledger.Used(), released, 2*tileBytes)
	}
}

func TestSupervisorAdmitsImageTilesThroughThePixelCache(t *testing.T) {
	source := image.NewNRGBA(image.Rect(0, 0, 300, 200))
	for i := 0; i < len(source.Pix); i += 4 {
		copy(source.Pix[i:], []byte{200, 100, 50, 255})
	}
	s := newTestSupervisor(t)
	r := ImageRequest{Format: ImagePNG, Data: imageEncodedPNG(t, source), Scale: 1, Viewport: [2]float64{300, 300}, Visible: true}
	p := cachedImagePreview(t, s, r)
	if p.Width != 300 || p.Height != 200 || len(p.Tiles) != 2 {
		t.Fatalf("cached preview = %+v; want 300x200 in two tiles", p)
	}
	if got := p.Tiles[1].Pix[:4]; !bytes.Equal(got, []byte{200, 100, 50, 255}) {
		t.Fatalf("cached pixel = %v, want the opaque source colour", got)
	}
	if s.Pixels().Used() != 300*200*4 {
		t.Fatalf("pixel cache holds %d bytes, want %d", s.Pixels().Used(), 300*200*4)
	}
}

func cachedImagePreview(t *testing.T, s *Supervisor, r ImageRequest) *Preview {
	t.Helper()
	id, _, err := s.Image(r)
	if err != nil {
		t.Fatal(err)
	}
	o := awaitOutcome(t, s, id, 2*time.Minute)
	if o.Err != nil || o.Image != nil || o.Preview != PreviewKeyOf(r) {
		t.Fatalf("image outcome = %+v; want a cache key and no pixels outside the cache", o)
	}
	p, ok := s.Pixels().Preview(o.Preview)
	if !ok {
		t.Fatalf("pixel cache lacks the preview %+v", o.Preview)
	}
	return p
}
