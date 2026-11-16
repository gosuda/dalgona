package owners

type Left struct {
	n int
}

type Right struct {
	n int
}

func (l *Left) Push(n int) {
	l.n += n
}

func (r *Right) Push(n int) {
	r.n += n
}

func (l Left) Peek() int {
	return l.n
}

func (r Right) Peek() int {
	return r.n
}

func (l *Left) Pop() int {
	l.n--
	return l.n
}

func (r *Right) Pop() int {
	r.n--
	return r.n
}

func Drain(l *Left, r *Right) int {
	return l.Pop() + r.Pop()
}
