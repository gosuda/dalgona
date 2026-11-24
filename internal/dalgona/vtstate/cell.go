package vtstate

type ColorKind uint8

const (
	ColorDefault ColorKind = iota
	ColorIndexed
	ColorRGB
)

type Color struct {
	Kind  ColorKind
	Index uint8
	Red   uint8
	Green uint8
	Blue  uint8
}

func IndexedColor(index uint8) Color {
	return Color{Kind: ColorIndexed, Index: index}
}

func RGBColor(red, green, blue uint8) Color {
	return Color{Kind: ColorRGB, Red: red, Green: green, Blue: blue}
}

type Underline uint8

const (
	UnderlineNone Underline = iota
	UnderlineSingle
	UnderlineDouble
	UnderlineCurly
	UnderlineDotted
	UnderlineDashed
)

type Attributes uint8

const (
	AttrBold Attributes = 1 << iota
	AttrFaint
	AttrItalic
	AttrBlink
	AttrRapidBlink
	AttrReverse
	AttrConceal
	AttrStrikethrough
)

type Style struct {
	Foreground     Color
	Background     Color
	UnderlineColor Color
	Underline      Underline
	Attributes     Attributes
}

type Link struct {
	URI string
	ID  string
}

type Cell struct {
	Text  string
	Link  *Link
	Style Style
	Width uint8
}

func (c Cell) Equal(o Cell) bool {
	return c.Text == o.Text && c.Width == o.Width && c.Style == o.Style && sameLink(c.Link, o.Link)
}

func (c Cell) Continuation() bool {
	return c.Width == 0
}

func sameLink(a, b *Link) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

func blankCell(background Color) Cell {
	return Cell{Text: " ", Width: 1, Style: Style{Background: background}}
}

func (c Cell) blanked() Cell {
	c.Text = " "
	c.Width = 1
	return c
}

func (c Cell) isBlank() bool {
	return c.Text == " " && c.Width == 1 && c.Style == Style{} && c.Link == nil
}
