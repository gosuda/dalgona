package platform

import (
	"errors"
	"image"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

func uiaRange(start, end int) textinput.Range {
	return textinput.Range{Start: start, End: end}
}

func TestUIATextUnitsFollowTheTextUnitEnumeration(t *testing.T) {
	want := map[int32]uiaTextUnit{
		0: uiaByCharacter,
		1: uiaByDocument,
		2: uiaByWord,
		3: uiaByLine,
		4: uiaByParagraph,
		5: uiaByDocument,
		6: uiaByDocument,
	}
	for unit := int32(-1); unit <= 7; unit++ {
		got, ok := uiaTextUnits[unit]
		expected, known := want[unit]
		if ok != known || got != expected {
			t.Errorf("TextUnit %d maps to %d (%t), want %d (%t)", unit, got, ok, expected, known)
		}
	}
}

func TestUIACharacterUnitStepsOverGraphemeClusters(t *testing.T) {
	text := "e\u0301x\U0001F469\u200d\U0001F4BB!"
	steps := map[int]int{0: 3, 3: 4, 4: 15, 15: 16, 16: 16}
	for from, want := range steps {
		if got := uiaByCharacter.next(text, from); got != want {
			t.Errorf("next character after %d = %d, want %d", from, got, want)
		}
	}
	if got, moved := uiaMove(text, uiaRange(0, 0), uiaByCharacter, 3); got != uiaRange(15, 15) || moved != 3 {
		t.Errorf("moving the caret three characters reached %v after %d moves", got, moved)
	}
	if got, moved := uiaMove(text, uiaRange(0, 0), uiaByCharacter, 10); got != uiaRange(16, 16) || moved != 4 {
		t.Errorf("moving the caret past the end reached %v after %d moves, want the end after 4", got, moved)
	}
	if got, moved := uiaMove(text, uiaRange(16, 16), uiaByCharacter, -2); got != uiaRange(4, 4) || moved != -2 {
		t.Errorf("moving the caret back two characters reached %v after %d moves", got, moved)
	}
}

func TestUIAWordUnitIncludesTrailingBreakCharacters(t *testing.T) {
	text := "Hello, world  again\nnext"
	expand := map[int]textinput.Range{
		0:  uiaRange(0, 5),
		3:  uiaRange(0, 5),
		5:  uiaRange(5, 7),
		8:  uiaRange(7, 14),
		13: uiaRange(7, 14),
		16: uiaRange(14, 20),
		22: uiaRange(20, 24),
		24: uiaRange(20, 24),
	}
	for at, want := range expand {
		if got := uiaExpand(text, uiaRange(at, at), uiaByWord); got != want {
			t.Errorf("word around %d = %v (%q), want %v (%q)", at, got, text[got.Start:got.End], want, text[want.Start:want.End])
		}
	}
	if got, moved := uiaMove(text, uiaRange(0, 5), uiaByWord, 2); got != uiaRange(7, 14) || moved != 2 {
		t.Errorf("moving the first word by two words reached %v after %d moves", got, moved)
	}
}

func TestUIAWordUnitFollowsUnicodeWordBoundariesInCJK(t *testing.T) {
	text := "漢字テスト 한국어"
	expand := map[int]textinput.Range{0: uiaRange(0, 3), 3: uiaRange(3, 6), 9: uiaRange(6, 16), 15: uiaRange(6, 16), 19: uiaRange(16, 25), 25: uiaRange(16, 25)}
	for at, want := range expand {
		if got := uiaExpand(text, uiaRange(at, at), uiaByWord); got != want {
			t.Errorf("word around %d = %v (%q), want %v (%q)", at, got, text[got.Start:got.End], want, text[want.Start:want.End])
		}
	}
	if got, moved := uiaMoveOffset(text, 0, uiaByWord, 3); got != 16 || moved != 3 {
		t.Errorf("three words forward reached %d after %d moves, want 16 after 3", got, moved)
	}
	if got, moved := uiaMoveOffset(text, 25, uiaByWord, -2); got != 6 || moved != -2 {
		t.Errorf("two words back reached %d after %d moves, want 6 after 2", got, moved)
	}
	prepended := "\u0d4e!x"
	if got := uiaByWord.next(prepended, 0); got != 4 {
		t.Errorf("a word boundary inside a prepended cluster moved to %d, want the cluster end 4", got)
	}
}

func TestUIALineAndParagraphUnitsBreakAfterLineFeeds(t *testing.T) {
	text := "one\ntwo\n\nfour"
	lines := map[int]textinput.Range{0: uiaRange(0, 4), 5: uiaRange(4, 8), 8: uiaRange(8, 9), 10: uiaRange(9, 13), 13: uiaRange(9, 13)}
	for at, want := range lines {
		if got := uiaExpand(text, uiaRange(at, at), uiaByLine); got != want {
			t.Errorf("line around %d = %v, want %v", at, got, want)
		}
	}
	paragraphs := map[int]textinput.Range{0: uiaRange(0, 4), 5: uiaRange(4, 9), 8: uiaRange(4, 9), 10: uiaRange(9, 13)}
	for at, want := range paragraphs {
		if got := uiaExpand(text, uiaRange(at, at), uiaByParagraph); got != want {
			t.Errorf("paragraph around %d = %v, want %v with the empty line merged", at, got, want)
		}
	}
}

func TestUIAMoveNormalizesNonDegenerateRanges(t *testing.T) {
	text := "one\ntwo\nthree"
	cases := []struct {
		from  textinput.Range
		count int32
		want  textinput.Range
		moved int32
	}{
		{uiaRange(1, 2), 1, uiaRange(4, 8), 1},
		{uiaRange(1, 2), 5, uiaRange(8, 13), 2},
		{uiaRange(8, 13), -1, uiaRange(4, 8), -1},
		{uiaRange(1, 2), -1, uiaRange(1, 2), 0},
		{uiaRange(9, 11), 0, uiaRange(9, 11), 0},
	}
	for _, c := range cases {
		if got, moved := uiaMove(text, c.from, uiaByLine, c.count); got != c.want || moved != c.moved {
			t.Errorf("move %v by %d lines = %v after %d, want %v after %d", c.from, c.count, got, moved, c.want, c.moved)
		}
	}
}

func TestUIAMoveEndpointCollapsesACrossedEndpoint(t *testing.T) {
	text := "one\ntwo\nthree"
	cases := []struct {
		endpoint int32
		unit     uiaTextUnit
		count    int32
		want     textinput.Range
		moved    int32
	}{
		{uiaEndpointStart, uiaByLine, 2, uiaRange(13, 13), 2},
		{uiaEndpointEnd, uiaByCharacter, -2, uiaRange(4, 6), -2},
		{uiaEndpointEnd, uiaByLine, -10, uiaRange(0, 0), -2},
		{uiaEndpointStart, uiaByCharacter, -1, uiaRange(3, 8), -1},
	}
	for _, c := range cases {
		if got, moved := uiaMoveEndpoint(text, uiaRange(4, 8), c.endpoint, c.unit, c.count); got != c.want || moved != c.moved {
			t.Errorf("move endpoint %d by %d = %v after %d, want %v after %d", c.endpoint, c.count, got, moved, c.want, c.moved)
		}
	}
}

func TestUIAEndpointsFollowTextPatternRangeEndpoint(t *testing.T) {
	for endpoint, want := range map[int32]bool{-1: false, 0: true, 1: true, 2: false} {
		if got := uiaValidEndpoint(endpoint); got != want {
			t.Errorf("endpoint %d valid %t, want %t", endpoint, got, want)
		}
	}
	if got := uiaWithEndpoint(uiaRange(4, 8), uiaEndpointEnd, 2); got != uiaRange(2, 2) {
		t.Errorf("moving the end before the start gave %v", got)
	}
	if got := uiaEndpointOf(uiaRange(4, 8), uiaEndpointEnd); got != 8 {
		t.Errorf("end endpoint %d", got)
	}
}

func TestUIAExpandToDocumentCoversAllText(t *testing.T) {
	text := "abc\ndef"
	for _, at := range []int{0, 2, 5, 7} {
		if got := uiaExpand(text, uiaRange(at, at), uiaTextUnits[6]); got != uiaRange(0, 7) {
			t.Errorf("document around %d = %v", at, got)
		}
		if got := uiaExpand(text, uiaRange(at, at), uiaTextUnits[1]); got != uiaRange(0, 7) {
			t.Errorf("uniform format around %d = %v, want the whole document", at, got)
		}
	}
	if got := uiaExpand("abc", uiaRange(3, 3), uiaByCharacter); got != uiaRange(2, 3) {
		t.Errorf("character at the end = %v, want the last character", got)
	}
	if got := uiaExpand("", uiaRange(0, 0), uiaByWord); got != uiaRange(0, 0) {
		t.Errorf("word in empty text = %v", got)
	}
}

func TestUIAGetTextHonorsMaxLengthAndTheTextObjectLimit(t *testing.T) {
	text := "a\U0001F600b"
	cases := map[int]string{-1: text, 0: "", 1: "a", 2: "a", 3: "a\U0001F600", 4: text, 9: text}
	for limit, want := range cases {
		if got := uiaRangeText(text, uiaRange(0, len(text)), limit); got != want {
			t.Errorf("text with maxLength %d = %q, want %q", limit, got, want)
		}
	}
	long := strings.Repeat("x", 70000)
	if got := uiaRangeText(long, uiaRange(0, len(long)), -1); len(got) != 64<<10 {
		t.Errorf("one callback returned %d bytes, want the 64 KiB text object", len(got))
	}
}

func TestUIAFindTextRespectsCaseAndClusters(t *testing.T) {
	cases := []struct {
		text     string
		within   textinput.Range
		needle   string
		backward bool
		fold     bool
		want     textinput.Range
		found    bool
	}{
		{"Alpha beta ALPHA", uiaRange(0, 16), "alpha", false, false, textinput.Range{}, false},
		{"Alpha beta ALPHA", uiaRange(0, 16), "alpha", false, true, uiaRange(0, 5), true},
		{"Alpha beta ALPHA", uiaRange(0, 16), "alpha", true, true, uiaRange(11, 16), true},
		{"Alpha beta ALPHA", uiaRange(1, 16), "alpha", false, true, uiaRange(11, 16), true},
		{"Alpha beta ALPHA", uiaRange(0, 15), "alpha", true, true, uiaRange(0, 5), true},
		{"e\u0301", uiaRange(0, 3), "e", false, false, textinput.Range{}, false},
		{"e\u0301", uiaRange(0, 3), "e\u0301", false, false, uiaRange(0, 3), true},
		{"\u212A", uiaRange(0, 3), "k", false, true, uiaRange(0, 3), true},
		{"abc", uiaRange(0, 3), "", false, false, textinput.Range{}, false},
	}
	for _, c := range cases {
		got, found := uiaFind(c.text, c.within, c.needle, c.backward, c.fold)
		if got != c.want || found != c.found {
			t.Errorf("find %q in %q%v backward %t fold %t = %v %t, want %v %t", c.needle, c.text, c.within, c.backward, c.fold, got, found, c.want, c.found)
		}
	}
}

func TestUIASnapMovesOffsetsToClusterStarts(t *testing.T) {
	text := "e\u0301x"
	for at, want := range map[int]int{-3: 0, 0: 0, 1: 0, 2: 0, 3: 3, 4: 4, 99: 4} {
		if got := uiaSnap(text, at); got != want {
			t.Errorf("snap %d = %d, want %d", at, got, want)
		}
	}
	if got := uiaSnapRange(text, uiaRange(4, 2)); got != uiaRange(0, 4) {
		t.Errorf("a reversed stale range snapped to %v", got)
	}
}

func TestUIAPointOffsetUsesLineGeometry(t *testing.T) {
	n := geometryNode()
	for p, want := range map[image.Point]int{image.Pt(45, 110): 3, image.Pt(5, 130): 11, image.Pt(50, 300): 12, image.Pt(200, 110): 9} {
		if got := uiaPointOffset(n, p); got != want {
			t.Errorf("point %v maps to offset %d, want %d", p, got, want)
		}
	}
	n.Lines, n.Bounds = nil, image.Rect(10, 10, 110, 50)
	for p, want := range map[image.Point]int{image.Pt(20, 20): 0, image.Pt(20, 5): 0, image.Pt(20, 50): 13, image.Pt(20, 90): 13} {
		if got := uiaPointOffset(n, p); got != want {
			t.Errorf("without geometry, point %v maps to offset %d, want %d", p, got, want)
		}
	}
}

func TestUIABoundingRectanglesFollowVisibleLines(t *testing.T) {
	n := geometryNode()
	n.Bounds = image.Rect(0, 90, 85, 130)
	if got := uiaVisibleBoxes(n, uiaRange(0, 13)); !slices.Equal(got, []image.Rectangle{image.Rect(10, 100, 85, 120), image.Rect(10, 120, 30, 130)}) {
		t.Errorf("visible boxes %v, want each line clipped to the element", got)
	}
	if got := uiaVisibleBoxes(n, uiaRange(3, 7)); !slices.Equal(got, []image.Rectangle{image.Rect(40, 100, 60, 120)}) {
		t.Errorf("emoji box %v", got)
	}
	if got := uiaVisibleBoxes(n, uiaRange(4, 4)); len(got) != 0 {
		t.Errorf("a degenerate range has boxes %v", got)
	}
	n.Bounds = image.Rect(0, 90, 200, 119)
	if got := uiaVisibleBoxes(n, uiaRange(11, 13)); len(got) != 0 {
		t.Errorf("a line scrolled out of the element has boxes %v", got)
	}
	n.Lines = nil
	if got := uiaVisibleBoxes(n, uiaRange(0, 13)); !slices.Equal(got, []image.Rectangle{n.Bounds}) {
		t.Errorf("text without line geometry has boxes %v, want the element box", got)
	}
	if got := uiaVisibleBoxes(n, uiaRange(4, 4)); len(got) != 0 {
		t.Errorf("a degenerate range without line geometry has boxes %v", got)
	}
}

func TestUIALimitedRangeDoesNotFetchBeyondItsPrefix(t *testing.T) {
	tree, snap := publishedTree(t, 1, []SemanticNode{
		{ID: "doc", Role: RoleDocument, TextContainer: true},
		{ID: "composer", Parent: "doc", Role: RoleDocument, TextGroup: "doc", Value: "a😀"},
		{ID: "last", Parent: "doc", Role: RoleDocument, TextGroup: "doc", TextBefore: "\n", TextState: TextUncached, TextRevision: "r1"},
	})
	view := tree.textView(snap, "doc")
	for limit, want := range map[int]string{2: "a", 3: "a😀", 4: "a😀\n"} {
		if got, err := view.readUTF16(view.whole(), limit); err != nil || got != want {
			t.Fatalf("limit %d returned %q, %v; want cached prefix %q", limit, got, err, want)
		}
	}
	select {
	case intent := <-tree.Intents():
		t.Fatalf("cached prefix enqueued an unexpected request: %+v", intent)
	default:
	}
	if got, err := view.readUTF16(view.whole(), 5); got != "" || !errors.Is(err, ErrAccessibilityBusy) {
		t.Fatalf("limit 5 returned %q, %v; want busy", got, err)
	}
}
