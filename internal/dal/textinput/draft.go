package textinput

import "unicode/utf8"

type Range struct {
	Start, End int
}

type Draft struct {
	Text      string
	Selection Range
}

func CheckRange(s string, r Range) error {
	if r.Start > r.End || !isBoundary(s, r.Start) || !isBoundary(s, r.End) {
		return InvalidOffset
	}
	return nil
}

func PaintRange(s string, r Range) (Range, error) {
	if r.Start < 0 || r.Start > r.End || r.End > len(s) || !isScalarBoundary(s, r.Start) || !isScalarBoundary(s, r.End) {
		return Range{}, InvalidOffset
	}
	paint := r
	if r.Start < len(s) {
		paint.Start, _ = clusterAt(s, r.Start)
	}
	paint.End = boundaryAtOrAfter(s, r.End)
	return paint, nil
}

func (d Draft) Replace(p Policy, candidate string) (Draft, error) {
	if !utf8.ValidString(candidate) {
		return d, InvalidEncoding
	}
	if err := CheckRange(d.Text, d.Selection); err != nil {
		return d, err
	}
	inserted := p.adapt(candidate)
	if reason := p.rejectContent(inserted); reason != "" {
		return d, reason
	}
	start, end := d.Selection.Start, d.Selection.End
	unit := p.Limit.Unit
	if Count(d.Text[:start], unit)+Count(inserted, unit)+Count(d.Text[end:], unit) > p.Limit.Max {
		return d, TextTooLarge
	}
	return edited(d.Text[:start]+inserted+d.Text[end:], start+len(inserted)), nil
}

func (d Draft) DeleteBackward() (Draft, error) {
	if err := CheckRange(d.Text, d.Selection); err != nil {
		return d, err
	}
	removed := d.Selection
	if removed.Start == removed.End {
		if removed.Start == 0 {
			return d, nil
		}
		removed.Start, _ = clusterAt(d.Text, removed.Start-1)
	}
	return edited(d.Text[:removed.Start]+d.Text[removed.End:], removed.Start), nil
}

func (d Draft) DeleteForward() (Draft, error) {
	if err := CheckRange(d.Text, d.Selection); err != nil {
		return d, err
	}
	removed := d.Selection
	if removed.Start == removed.End {
		if removed.End == len(d.Text) {
			return d, nil
		}
		removed.End = clusterEnd(d.Text, removed.End)
	}
	return edited(d.Text[:removed.Start]+d.Text[removed.End:], removed.Start), nil
}

func edited(text string, caret int) Draft {
	caret = boundaryAtOrAfter(text, caret)
	return Draft{Text: text, Selection: Range{Start: caret, End: caret}}
}

func isScalarBoundary(s string, i int) bool {
	return i == len(s) || utf8.RuneStart(s[i])
}
