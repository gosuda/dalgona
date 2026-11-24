package state

import (
	"errors"
	"testing"
)

func span(first, last uint64) envelope {
	return envelope{first: first, last: last, thread: "t", method: "m"}
}

func TestSequencerAcceptsContiguousRangesAndRefusesRepeatsAndGaps(t *testing.T) {
	cases := []struct {
		name string
		in   []envelope
		want error
	}{
		{"contiguous ranges", []envelope{span(1, 1), span(2, 4), span(5, 5)}, nil},
		{"a repeated ordinal", []envelope{span(1, 2), span(2, 2)}, errOverlap},
		{"a range overlapping the last one", []envelope{span(1, 3), span(3, 4)}, errOverlap},
		{"a skipped ordinal", []envelope{span(1, 1), span(3, 3)}, errGap},
		{"a stream starting after one", []envelope{span(2, 2)}, errGap},
	}
	for _, c := range cases {
		s := newSequencer()
		var err error
		for _, e := range c.in {
			if err = s.accept(e); err != nil {
				break
			}
		}
		if !errors.Is(err, c.want) {
			t.Errorf("%s: accept = %v, want %v", c.name, err, c.want)
		}
	}
}
