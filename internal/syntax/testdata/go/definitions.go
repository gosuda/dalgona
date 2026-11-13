package sample

type Shape struct {
	W int
}

type Named interface {
	Name() string
}

type Count int

type Alias = Shape

const Limit = 8

var Default = Shape{}

func Area(s Shape) int {
	total := s.W
	return total
}

func (s Shape) Double() Shape {
	return s
}
