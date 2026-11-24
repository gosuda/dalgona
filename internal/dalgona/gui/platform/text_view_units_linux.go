//go:build linux

package platform

import "github.com/gosuda/dalgona/internal/dal/inputtext"

func (v textView) length(unit inputtext.Unit) (int, error) {
	if len(v.parts()) == 0 {
		return 0, nil
	}
	return v.offset(v.whole().End, unit)
}

func (v textView) unitOffsets(r TextRange, unit inputtext.Unit) (int, int, error) {
	start, err := v.offset(r.Start, unit)
	if err != nil {
		return 0, 0, err
	}
	end, err := v.offset(r.End, unit)
	return start, end, err
}

func (v textView) readUnits(start, end int, unit inputtext.Unit) (string, error) {
	from, err := v.locate(start, unit, true)
	if err != nil {
		return "", err
	}
	to := v.whole().End
	if end >= 0 && end-start < TextObjectLimit {
		to, err = v.locate(end, unit, true)
		if err != nil {
			return "", err
		}
	}
	return v.read(TextRange{Start: from, End: to})
}
