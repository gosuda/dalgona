package math

type Span struct {
	Start, End int
}

func (s Span) Range() Span { return s }

type Node interface {
	Range() Span
}

type Class uint8

const (
	ClassOrd Class = iota
	ClassOp
	ClassBin
	ClassRel
	ClassOpen
	ClassClose
	ClassPunct
	ClassInner
)

type Formula struct {
	Source string
	Body   []Node
	Tag    *Tag
	Nodes  int
}

type Symbol struct {
	Span
	Command string
	Value   rune
	Class   Class
}

type Number struct {
	Span
	Digits string
}

type Group struct {
	Span
	Body []Node
}

type Field struct {
	Span
	Body []Node
}

type Scripts struct {
	Span
	Base   Node
	Primes int
	Sub    *Field
	Sup    *Field
}

type Fraction struct {
	Span
	Command     string
	Numerator   []Node
	Denominator []Node
}

type Radical struct {
	Span
	Index *Field
	Body  []Node
}

type Delimiter struct {
	Span
	Command string
	Value   rune
}

type Fence struct {
	Span
	Left     Delimiter
	Middles  []Delimiter
	Segments [][]Node
	Right    Delimiter
}

type SizedDelimiter struct {
	Span
	Command   string
	Delimiter Delimiter
}

type Limits uint8

const (
	LimitsDefault Limits = iota
	LimitsAbove
	LimitsBeside
)

type Operator struct {
	Span
	Command string
	Name    string
	Value   rune
	Limits  Limits
}

type Accent struct {
	Span
	Command string
	Body    []Node
}

type Alphabet struct {
	Span
	Command string
	Body    []Node
}

type Text struct {
	Span
	Value string
}

type Space struct {
	Span
	Command string
	Amount  float32
	Unit    string
}

type StyleChange struct {
	Span
	Command string
}

type Environment struct {
	Span
	Name    string
	Columns string
	Pairs   int
	Rows    []Row
}

type Row struct {
	Cells [][]Node
	Tag   *Tag
}

type Tag struct {
	Span
	Star bool
	Text string
}
