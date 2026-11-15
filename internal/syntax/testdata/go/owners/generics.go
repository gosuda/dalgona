package owners

type Box[T any] struct {
	item T
}

func (b Box[T]) Get() T {
	return b.item
}

func (b *Box[T]) Set(item T) {
	b.item = item
}

func (Box[_]) Empty() bool {
	return false
}

type Pair[K comparable, V any] struct {
	key K
	val V
}

func (p Pair[K, V]) Key() K {
	return p.key
}

func (p *Pair[K, V]) Put(key K, val V) {
	p.key = key
	p.val = val
}

func (p (*Pair[K, V])) Both() (K, V) {
	return p.key, p.val
}

func (Pair[_, V]) Zero() V {
	var zero V
	return zero
}

func Make[K comparable, V any](key K, val V) Pair[K, V] {
	return Pair[K, V]{key: key, val: val}
}
