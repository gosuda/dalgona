package document

import (
	"container/list"
	"fmt"
	"hash/fnv"
)

type GeometryKey struct {
	Block      Key
	Row        int
	Revision   Revision
	Width      int
	Scale      float32
	Fonts      string
	Shaping    string
	Roles      uint32
	Projection uint64
}

func NewGeometryKey(d *Document, block, row int) GeometryKey {
	b := d.blocks[block]
	key := GeometryKey{Block: b.Key, Row: row, Projection: b.Fingerprint}
	if key.Projection == 0 {
		key.Projection = projectionFingerprint(b.Projection)
	}
	if b.Key.Provisional {
		key.Revision = d.revision
	}
	return key
}

func projectionFingerprint(p Projection) uint64 {
	h := fnv.New64a()
	_, _ = fmt.Fprintf(h, "%s\x00%d\x00", p.Text, len(p.Runs))
	for _, r := range p.Runs {
		_, _ = fmt.Fprintf(h, "%d/%d/%d/%d/%d/%d/%d\x00",
			r.Text.Start, r.Text.End, r.Source.Start, r.Source.End, r.Kind, r.Style, r.Link)
	}
	return h.Sum64()
}

type Cache[V any] struct {
	budget int
	used   int
	items  map[GeometryKey]*list.Element
	order  *list.List
}

type cached[V any] struct {
	key   GeometryKey
	value V
	size  int
}

func NewCache[V any](budget int) *Cache[V] {
	return &Cache[V]{budget: budget, items: map[GeometryKey]*list.Element{}, order: list.New()}
}

func (c *Cache[V]) Get(key GeometryKey) (V, bool) {
	element, ok := c.items[key]
	if !ok {
		var zero V
		return zero, false
	}
	c.order.MoveToFront(element)
	return element.Value.(*cached[V]).value, true
}

func (c *Cache[V]) Put(key GeometryKey, value V, size int) bool {
	if size > c.budget {
		return false
	}
	if element, ok := c.items[key]; ok {
		c.used -= element.Value.(*cached[V]).size
		c.order.Remove(element)
		delete(c.items, key)
	}
	for c.used+size > c.budget {
		oldest := c.order.Back()
		item := oldest.Value.(*cached[V])
		c.used -= item.size
		c.order.Remove(oldest)
		delete(c.items, item.key)
	}
	c.items[key] = c.order.PushFront(&cached[V]{key: key, value: value, size: size})
	c.used += size
	return true
}

func (c *Cache[V]) Used() int {
	return c.used
}

func (c *Cache[V]) Len() int {
	return len(c.items)
}
