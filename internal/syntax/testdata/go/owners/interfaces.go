package owners

type Reader interface {
	Read(p []byte) (int, error)
}

type Closer interface {
	Close() error
}

type ReadCloser interface {
	Reader
	Closer
	Name() string
}

type Sizer interface {
	Size() int
	Len() int
}

type Number interface {
	~int | ~int64
	String() string
}

type Container[T any] interface {
	Put(item T)
	Take() T
}

type Base struct {
	id int
}

func (b Base) ID() int {
	return b.id
}

type Derived struct {
	Base
	*Counter
	name string
}

func (d Derived) Name() string {
	return d.name
}

func (d *Derived) Close() error {
	return nil
}
