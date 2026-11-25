package figure

import (
	"container/list"
	"crypto/sha256"
	"sync"
)

type PixelLedger struct {
	mu      sync.Mutex
	limit   int
	used    int
	recency list.List
	charges map[any]*list.Element
}

type pixelCharge struct {
	key     any
	bytes   int
	release func()
}

var SharedPixels = NewPixelLedger(PixelCacheLimit)

func NewPixelLedger(limit int) *PixelLedger {
	return &PixelLedger{limit: limit, charges: map[any]*list.Element{}}
}

func (l *PixelLedger) Limit() int { return l.limit }

func (l *PixelLedger) Used() int {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.used
}

func (l *PixelLedger) Charge(key any, bytes int, release func()) error {
	if bytes > l.limit {
		return fail(FailureLimit, "pixels need %d bytes; the shared pixel budget is %d", bytes, l.limit)
	}
	l.mu.Lock()
	l.remove(key)
	l.charges[key] = l.recency.PushFront(&pixelCharge{key: key, bytes: bytes, release: release})
	l.used += bytes
	var evicted []func()
	for l.used > l.limit {
		evicted = append(evicted, l.remove(l.recency.Back().Value.(*pixelCharge).key))
	}
	l.mu.Unlock()
	for _, release := range evicted {
		release()
	}
	return nil
}

func (l *PixelLedger) remove(key any) func() {
	e, ok := l.charges[key]
	if !ok {
		return func() {}
	}
	charge := l.recency.Remove(e).(*pixelCharge)
	delete(l.charges, key)
	l.used -= charge.bytes
	return charge.release
}

func (l *PixelLedger) Touch(key any) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if e, ok := l.charges[key]; ok {
		l.recency.MoveToFront(e)
	}
}

func (l *PixelLedger) Discharge(key any) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.remove(key)
}

type Digest [sha256.Size]byte

func ImageDigest(data []byte) Digest { return sha256.Sum256(data) }

type PreviewKey struct {
	Image Digest
	Side  int
}

func PreviewKeyOf(r ImageRequest) PreviewKey {
	return PreviewKey{Image: ImageDigest(r.Data), Side: previewSide(r.Viewport[:], r.Scale)}
}

type TileKey struct {
	Preview PreviewKey
	X, Y    int
}

type previewShape struct {
	width, height, sourceWidth, sourceHeight int
}

type cacheTile struct {
	cache *PixelCache
	key   TileKey
}

type PixelCache struct {
	ledger   *PixelLedger
	mu       sync.Mutex
	used     int
	tiles    map[TileKey]Tile
	previews map[PreviewKey]previewShape
}

func NewPixelCache(ledger *PixelLedger) *PixelCache {
	return &PixelCache{ledger: ledger, tiles: map[TileKey]Tile{}, previews: map[PreviewKey]previewShape{}}
}

func (c *PixelCache) Used() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.used
}

func previewBytes(p *Preview) (int, error) {
	total := 0
	for _, t := range p.Tiles {
		if t.Width < 1 || t.Height < 1 || t.Width > TileSideLimit || t.Height > TileSideLimit || len(t.Pix) != t.Width*t.Height*4 {
			return 0, fail(FailureProtocol, "preview tile %dx%d carries %d bytes", t.Width, t.Height, len(t.Pix))
		}
		total += len(t.Pix)
	}
	return total, nil
}

func (c *PixelCache) Admit(key PreviewKey, p *Preview) error {
	total, err := previewBytes(p)
	if err != nil {
		return err
	}
	if total > c.ledger.Limit() {
		return fail(FailureLimit, "image preview needs %d bytes; the shared pixel budget is %d", total, c.ledger.Limit())
	}
	if c.HasPreview(key) {
		return nil
	}
	c.releasePreview(key)
	c.mu.Lock()
	c.previews[key] = previewShape{width: p.Width, height: p.Height, sourceWidth: p.SourceWidth, sourceHeight: p.SourceHeight}
	keys := make([]TileKey, 0, len(p.Tiles))
	for _, t := range p.Tiles {
		tileKey := TileKey{Preview: key, X: int(t.Bounds.Min.X), Y: int(t.Bounds.Min.Y)}
		c.tiles[tileKey] = t
		c.used += len(t.Pix)
		keys = append(keys, tileKey)
	}
	c.mu.Unlock()
	for i, tileKey := range keys {
		if err := c.ledger.Charge(cacheTile{cache: c, key: tileKey}, len(p.Tiles[i].Pix), func() { c.forget(tileKey) }); err != nil {
			return err
		}
	}
	return nil
}

func (c *PixelCache) forget(key TileKey) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if t, ok := c.tiles[key]; ok {
		delete(c.tiles, key)
		c.used -= len(t.Pix)
	}
	delete(c.previews, key.Preview)
}

func (c *PixelCache) Tile(key TileKey) (Tile, bool) {
	c.mu.Lock()
	t, ok := c.tiles[key]
	c.mu.Unlock()
	if ok {
		c.ledger.Touch(cacheTile{cache: c, key: key})
	}
	return t, ok
}

func (c *PixelCache) HasPreview(key PreviewKey) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	_, ok := c.previews[key]
	return ok
}

func (c *PixelCache) Preview(key PreviewKey) (*Preview, bool) {
	c.mu.Lock()
	shape, ok := c.previews[key]
	if !ok {
		c.mu.Unlock()
		return nil, false
	}
	p := &Preview{Width: shape.width, Height: shape.height, SourceWidth: shape.sourceWidth, SourceHeight: shape.sourceHeight}
	var keys []TileKey
	for y := 0; y < shape.height; y += TileSideLimit {
		for x := 0; x < shape.width; x += TileSideLimit {
			k := TileKey{Preview: key, X: x, Y: y}
			p.Tiles = append(p.Tiles, c.tiles[k])
			keys = append(keys, k)
		}
	}
	c.mu.Unlock()
	for _, k := range keys {
		c.ledger.Touch(cacheTile{cache: c, key: k})
	}
	return p, true
}

func (c *PixelCache) releasePreview(key PreviewKey) {
	c.mu.Lock()
	var dropped []TileKey
	for k, t := range c.tiles {
		if k.Preview == key {
			delete(c.tiles, k)
			c.used -= len(t.Pix)
			dropped = append(dropped, k)
		}
	}
	delete(c.previews, key)
	c.mu.Unlock()
	for _, k := range dropped {
		c.ledger.Discharge(cacheTile{cache: c, key: k})
	}
}
