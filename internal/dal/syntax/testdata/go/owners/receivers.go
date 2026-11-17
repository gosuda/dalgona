package owners

type Counter struct {
	n int
}

func (c Counter) Value() int {
	return c.n
}

func (c *Counter) Add(delta int) {
	c.n += delta
}

func (Counter) Kind() string {
	return "counter"
}

func (*Counter) Reset() {}

func (_ Counter) Blank() {}

func (c (Counter)) Wrapped() int {
	return c.n
}

func (c (*Counter)) WrappedPointer() {
	c.n++
}

func Standalone(c Counter) int {
	return c.Value()
}

func Convert[T any](value T) T {
	return value
}

func (late *Late) Early() {}

func (late Late) Middle() int {
	return late.size
}

type Late struct {
	size int
}

func (late *Late) After() {}
