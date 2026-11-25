package document

import (
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

type Step uint8

const (
	StepGrapheme Step = iota + 1
	StepWord
)

type Selection struct {
	Anchor Position
	Focus  Position
}

func Caret(p Position) Selection {
	return Selection{Anchor: p, Focus: p}
}

func (s Selection) Extend(focus Position) Selection {
	return Selection{Anchor: s.Anchor, Focus: focus}
}

func (s Selection) Collapsed() bool {
	return s.Anchor.Document == s.Focus.Document && s.Anchor.Offset == s.Focus.Offset && s.Anchor.Block == s.Focus.Block
}

func (d *Document) Next(p Position, step Step) (Position, bool, error) {
	i, t, err := d.locate(p)
	if err != nil {
		return p, false, err
	}
	projection := d.blocks[i].Projection
	if t >= len(projection.Text) {
		if i+1 == len(d.blocks) {
			return p, false, nil
		}
		return d.at(i+1, 0, After), true, nil
	}
	next := nextCluster(projection.Text, t)
	if step == StepWord {
		next = nextWordEnd(projection.Text, t)
	}
	return d.at(i, snap(projection, next, Before), Before), true, nil
}

func (d *Document) Previous(p Position, step Step) (Position, bool, error) {
	i, t, err := d.locate(p)
	if err != nil {
		return p, false, err
	}
	projection := d.blocks[i].Projection
	if t == 0 {
		if i == 0 {
			return p, false, nil
		}
		return d.at(i-1, len(d.blocks[i-1].Projection.Text), Before), true, nil
	}
	previous := previousCluster(projection.Text, t)
	if step == StepWord {
		previous = previousWordStart(projection.Text, t)
	}
	return d.at(i, snap(projection, previous, After), After), true, nil
}

func (d *Document) Word(p Position) (Selection, error) {
	i, t, err := d.locate(p)
	if err != nil {
		return Selection{}, err
	}
	projection := d.blocks[i].Projection
	start, end := wordAround(projection.Text, t)
	return Selection{
		Anchor: d.at(i, snap(projection, start, After), After),
		Focus:  d.at(i, snap(projection, end, Before), Before),
	}, nil
}

func (d *Document) BlockSelection(p Position) (Selection, error) {
	i, _, err := d.locate(p)
	if err != nil {
		return Selection{}, err
	}
	return Selection{
		Anchor: d.at(i, 0, After),
		Focus:  d.at(i, len(d.blocks[i].Projection.Text), Before),
	}, nil
}

func (d *Document) Compare(a, b Position) (int, error) {
	ai, at, err := d.locate(a)
	if err != nil {
		return 0, err
	}
	bi, bt, err := d.locate(b)
	if err != nil {
		return 0, err
	}
	if ai != bi {
		return ai - bi, nil
	}
	return at - bt, nil
}

func (d *Document) Text(from, to Position) (string, error) {
	fi, ft, err := d.locate(from)
	if err != nil {
		return "", err
	}
	ti, tt, err := d.locate(to)
	if err != nil {
		return "", err
	}
	if fi > ti || (fi == ti && ft > tt) {
		fi, ft, ti, tt = ti, tt, fi, ft
	}
	return d.ProjectedText(ProjectionPoint{Block: fi, Byte: ft}, ProjectionPoint{Block: ti, Byte: tt}), nil
}

type ProjectionPoint struct {
	Block, Byte int
}

func (d *Document) ProjectedText(from, to ProjectionPoint) string {
	fi, ft, ti, tt := from.Block, from.Byte, to.Block, to.Byte
	if fi == ti {
		return d.blocks[fi].Projection.Text[ft:tt]
	}
	left, right := d.textWindow(fi, fi, ft, ti, tt), d.textWindow(ti, fi, ft, ti, tt)
	if fi+1 == ti {
		return left + d.separator(fi) + right
	}
	var out strings.Builder
	for i := fi; i <= ti; i++ {
		out.WriteString(d.textWindow(i, fi, ft, ti, tt))
		if i < ti {
			out.WriteString(d.separator(i))
		}
	}
	return out.String()
}

func (d *Document) textWindow(i, fi, ft, ti, tt int) string {
	text := d.blocks[i].Projection.Text
	start, end := 0, len(text)
	if i == fi {
		start = ft
	}
	if i == ti {
		end = tt
	}
	return text[start:end]
}

func (d *Document) separator(i int) string {
	separator := "\n"
	if d.blankBetween(i) {
		separator = "\n\n"
	}
	if strings.HasSuffix(d.blocks[i].Projection.Text, "\n") {
		separator = separator[1:]
	}
	return separator
}

func (d *Document) blankBetween(i int) bool {
	gapStart := d.contentEnd(i)
	gapEnd := d.blocks[i+1].Span.Start
	if runs := d.blocks[i+1].Projection.Runs; len(runs) > 0 {
		gapEnd = runs[0].Source.Start
	}
	if gapEnd <= gapStart {
		return false
	}
	lines := strings.Split(d.source[gapStart:gapEnd], "\n")
	first := 1
	if gapStart == 0 || d.source[gapStart-1] == '\n' {
		first = 0
	}
	for _, line := range lines[first : len(lines)-1] {
		if strings.Trim(line, " \t\r>") == "" {
			return true
		}
	}
	return false
}

func (d *Document) Markdown(from, to Position) (string, error) {
	if from.Document != d.header.Identity || to.Document != d.header.Identity {
		return "", ErrForeign
	}
	if err := d.checkOffset(from.Offset, from.Affinity); err != nil {
		return "", err
	}
	if err := d.checkOffset(to.Offset, to.Affinity); err != nil {
		return "", err
	}
	start, end := min(from.Offset, to.Offset), max(from.Offset, to.Offset)
	return d.source[start:end], nil
}

func (d *Document) Find(query string, limit int) ([]Selection, int) {
	if query == "" {
		return nil, 0
	}
	var found []Selection
	count := 0
	for i := range d.blocks {
		count += d.findInBlock(i, query, &found, limit)
	}
	return found, count
}

func (d *Document) findInBlock(i int, query string, found *[]Selection, limit int) int {
	projection := d.blocks[i].Projection
	count := 0
	for at := 0; at <= len(projection.Text)-len(query); {
		hit := strings.Index(projection.Text[at:], query)
		if hit < 0 {
			break
		}
		start, end := at+hit, at+hit+len(query)
		_, width := utf8.DecodeRuneInString(projection.Text[start:])
		at = start + width
		if textinput.CheckRange(projection.Text, textinput.Range{Start: start, End: end}) != nil {
			continue
		}
		count++
		if len(*found) < limit {
			*found = append(*found, Selection{
				Anchor: d.at(i, snap(projection, start, After), After),
				Focus:  d.at(i, snap(projection, end, Before), Before),
			})
		}
	}
	return count
}
