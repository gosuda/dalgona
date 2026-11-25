package textinput

import (
	"errors"
	"slices"
	"testing"
)

func TestDeleteRemovesWholeClusters(t *testing.T) {
	family := "\U0001F468\u200d\U0001F469\u200d\U0001F467"
	cases := []struct {
		name     string
		draft    Draft
		backward bool
		want     Draft
	}{
		{"backspace a joined emoji", Draft{Text: "a" + family, Selection: Range{Start: 19, End: 19}}, true, Draft{Text: "a", Selection: Range{Start: 1, End: 1}}},
		{"backspace conjoining hangul", Draft{Text: "\u1100\u1161\u11a8x", Selection: Range{Start: 9, End: 9}}, true, Draft{Text: "x"}},
		{"backspace one flag of two", Draft{Text: "\U0001F1F0\U0001F1F7\U0001F1FA\U0001F1F8", Selection: Range{Start: 16, End: 16}}, true, Draft{Text: "\U0001F1F0\U0001F1F7", Selection: Range{Start: 8, End: 8}}},
		{"backspace an ideographic variation sequence", Draft{Text: "\U00020000\U000E0100", Selection: Range{Start: 8, End: 8}}, true, Draft{}},
		{"delete a combining sequence", Draft{Text: "e\u0301x"}, false, Draft{Text: "x"}},
		{"delete crlf as one cluster", Draft{Text: "\r\nx"}, false, Draft{Text: "x"}},
		{"delete a selection", Draft{Text: "abcd", Selection: Range{Start: 1, End: 3}}, false, Draft{Text: "ad", Selection: Range{Start: 1, End: 1}}},
		{"backspace at the start", Draft{Text: "ab"}, true, Draft{Text: "ab"}},
		{"delete at the end", Draft{Text: "ab", Selection: Range{Start: 2, End: 2}}, false, Draft{Text: "ab", Selection: Range{Start: 2, End: 2}}},
		{"merge after deletion keeps the caret on a boundary", Draft{Text: "\u094da\u0915", Selection: Range{Start: 4, End: 4}}, true, Draft{Text: "\u094d\u0915", Selection: Range{Start: 6, End: 6}}},
	}
	for _, c := range cases {
		got, err := deleteCluster(c.draft, c.backward)
		if err != nil || got != c.want {
			t.Errorf("%s: delete = %#v, %v; want %#v", c.name, got, err, c.want)
		}
	}
}

func TestDeleteRejectsSelectionInsideCluster(t *testing.T) {
	draft := Draft{Text: "e\u0301", Selection: Range{Start: 1, End: 1}}
	for _, backward := range []bool{true, false} {
		got, err := deleteCluster(draft, backward)
		if !errors.Is(err, InvalidOffset) || got != draft {
			t.Errorf("backward %t: delete = %#v, %v; want %v", backward, got, err, InvalidOffset)
		}
	}
}

func deleteCluster(d Draft, backward bool) (Draft, error) {
	if backward {
		return d.DeleteBackward()
	}
	return d.DeleteForward()
}

func TestNavigationVisitsEveryBoundaryInLogicalOrder(t *testing.T) {
	text := "a\r\ne\u0301\U0001F1F0\U0001F1F7\U0001F1FA\U0001F1F8\U0001F1EF\u0915\u094d\u0937\U0001F469\U0001F3FD\u200d\U0001F4BB\u1100\u1161\u11a8"
	want := []int{0, 1, 3, 6, 14, 22, 26, 35, 50, 59}
	if got := Boundaries(text); !slices.Equal(got, want) {
		t.Fatalf("Boundaries = %v, want %v", got, want)
	}
	forward := walkBoundaries(t, "NextClusterBoundary", text, 0, NextClusterBoundary)
	backward := walkBoundaries(t, "PreviousClusterBoundary", text, len(text), PreviousClusterBoundary)
	slices.Reverse(backward)
	if !slices.Equal(forward, want) || !slices.Equal(backward, want) {
		t.Fatalf("forward %v, backward %v, want %v", forward, backward, want)
	}
}

func walkBoundaries(t *testing.T, name, text string, at int, step func(string, int) (int, error)) []int {
	t.Helper()
	var visited []int
	for {
		visited = append(visited, at)
		next, err := step(text, at)
		if err != nil {
			t.Fatalf("%s(%d): %v", name, at, err)
		}
		if next == at {
			return visited
		}
		at = next
	}
}

func TestNavigationRejectsOffsetsOffBoundaries(t *testing.T) {
	text := "e\u0301\U0001F1F0\U0001F1F7"
	for _, at := range []int{-1, 1, 2, 4, 7, 12} {
		if _, err := NextClusterBoundary(text, at); !errors.Is(err, InvalidOffset) {
			t.Errorf("NextClusterBoundary(%d) = %v, want %v", at, err, InvalidOffset)
		}
		if _, err := PreviousClusterBoundary(text, at); !errors.Is(err, InvalidOffset) {
			t.Errorf("PreviousClusterBoundary(%d) = %v, want %v", at, err, InvalidOffset)
		}
	}
}

func TestConvertChecksScalarBoundaries(t *testing.T) {
	text := "a\u00e9\U0001F600\u1100"
	cases := []struct {
		offset   int
		from, to Unit
		want     int
	}{
		{0, Bytes, Scalars, 0},
		{3, Bytes, Scalars, 2},
		{7, Bytes, UTF16Units, 4},
		{10, Bytes, UTF16Units, 5},
		{4, UTF16Units, Bytes, 7},
		{3, Scalars, UTF16Units, 4},
		{2, UTF16Units, Scalars, 2},
		{3, Bytes, Bytes, 3},
	}
	for _, c := range cases {
		if got, err := Convert(text, c.offset, c.from, c.to); err != nil || got != c.want {
			t.Errorf("Convert(%d, %d to %d) = %d, %v; want %d", c.offset, c.from, c.to, got, err, c.want)
		}
	}
	invalid := []struct {
		offset int
		from   Unit
	}{{2, Bytes}, {4, Bytes}, {11, Bytes}, {-1, Bytes}, {3, UTF16Units}, {6, UTF16Units}, {5, Scalars}}
	for _, c := range invalid {
		if got, err := Convert(text, c.offset, c.from, Scalars); !errors.Is(err, InvalidOffset) {
			t.Errorf("Convert(%d from %d) = %d, %v; want %v", c.offset, c.from, got, err, InvalidOffset)
		}
	}
}

func TestPaintRangeExpandsOnlyThePaint(t *testing.T) {
	text := "xe\u0301y"
	got, err := PaintRange(text, Range{Start: 1, End: 2})
	if err != nil || got != (Range{Start: 1, End: 4}) {
		t.Fatalf("PaintRange = %v, %v; want [1,4)", got, err)
	}
	got, err = PaintRange(text, Range{Start: 2, End: 4})
	if err != nil || got != (Range{Start: 1, End: 4}) {
		t.Fatalf("PaintRange of a mark = %v, %v; want [1,4)", got, err)
	}
	for _, r := range []Range{{Start: 3, End: 4}, {Start: 2, End: 1}, {Start: 0, End: 6}, {Start: -1, End: 1}} {
		if got, err := PaintRange(text, r); !errors.Is(err, InvalidOffset) {
			t.Errorf("PaintRange(%v) = %v, %v; want %v", r, got, err, InvalidOffset)
		}
	}
}

func TestDecodeUTF16RejectsLoneSurrogates(t *testing.T) {
	got, err := DecodeUTF16([]uint16{0x0041, 0xFFFD, 0xD83D, 0xDE00, 0xD840, 0xDC00})
	if err != nil || got != "A\ufffd\U0001F600\U00020000" {
		t.Fatalf("DecodeUTF16 = %q, %v", got, err)
	}
	for _, units := range [][]uint16{{0xD83D, 0x0041}, {0x0041, 0xDE00}, {0xDE00, 0xD83D}, {0xD83D, 0xD83D, 0xDE00}} {
		if got, err := DecodeUTF16(units); !errors.Is(err, InvalidEncoding) || got != "" {
			t.Errorf("DecodeUTF16(%X) = %q, %v; want %v", units, got, err, InvalidEncoding)
		}
	}
}
