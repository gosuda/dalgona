package terminal

import (
	"fmt"
	"strings"
	"testing"
)

const selectionFixture = "first line\r\n" +
	"abcdefghijklmnopqrstuvwxyz\r\n" +
	"path /usr/lib/foo-bar.txt end\r\n" +
	"한글 단어 test\r\n"

func TestSelectionCopiesVisibleTextAcrossScrollbackAndWraps(t *testing.T) {
	replica := replicaOf(t, 20, 5, selectionFixture)
	if first, end := replica.Lines(); first != 0 || end != 7 {
		t.Fatalf("lines = %d..%d, want 0..7 with two rows in scrollback", first, end)
	}
	cases := []struct {
		name string
		sel  Selection
		want string
	}{
		{"characters join a soft wrap", Selection{Anchor: Point{1, 15}, Head: Point{2, 3}}, "pqrstuvwx"},
		{"characters break hard lines and trim padding", Selection{Anchor: Point{2, 1}, Head: Point{0, 6}}, "line\nabcdefghijklmnopqrstuv"},
		{"word follows a path across the wrap", Selection{Anchor: Point{3, 8}, Head: Point{3, 8}, Mode: SelectWords}, "/usr/lib/foo-bar.txt"},
		{"word from a wide continuation", Selection{Anchor: Point{5, 1}, Head: Point{5, 1}, Mode: SelectWords}, "한글"},
		{"line takes the whole logical line", Selection{Anchor: Point{4, 2}, Head: Point{4, 2}, Mode: SelectLines}, "path /usr/lib/foo-bar.txt end"},
		{"rectangle keeps columns", Selection{Anchor: Point{1, 2}, Head: Point{3, 5}, Mode: SelectRectangle}, "cdef\nwxyz\nth /"},
		{"wide cell is never split", Selection{Anchor: Point{5, 3}, Head: Point{5, 5}}, "글 단"},
	}
	for _, c := range cases {
		if got := replica.Text(replica.Extent(c.sel)); got != c.want {
			t.Fatalf("%s: copied %q, want %q", c.name, got, c.want)
		}
	}
}

func TestSelectionKeepsItsLinesWhenOutputScrolls(t *testing.T) {
	replica := replicaOf(t, 20, 5, selectionFixture)
	extent := replica.Extent(Selection{Anchor: Point{3, 0}, Head: Point{4, 8}})
	offset, ordinal := replica.Cursors()
	more := "one\r\ntwo\r\nthree\r\n"
	if err := replica.Apply(Read{Offset: offset, Next: offset + uint64(len(more)), Data: []byte(more), ResizeOrdinal: ordinal}); err != nil {
		t.Fatal(err)
	}
	if got, want := replica.Text(extent), "path /usr/lib/foo-bar.txt end"; got != want {
		t.Fatalf("after scrolling, copied %q, want %q", got, want)
	}
}

func TestSelectionKeepsItsLinesWhileHistoryEvicts(t *testing.T) {
	replica := replicaOf(t, 10, 5, "")
	feedReplica := func(text string) {
		offset, ordinal := replica.Cursors()
		if err := replica.Apply(Read{Offset: offset, Next: offset + uint64(len(text)), Data: []byte(text), ResizeOrdinal: ordinal}); err != nil {
			t.Fatal(err)
		}
	}
	var history strings.Builder
	for i := range 10010 {
		fmt.Fprintf(&history, "%d\r\n", i)
	}
	feedReplica(history.String())
	line := replica.ScreenTop()
	extent := replica.Extent(Selection{Anchor: Point{line, 0}, Head: Point{line, 9}})
	before := replica.Text(extent)
	evictedBefore, _ := replica.Lines()
	feedReplica("a\r\nb\r\nc\r\n")
	if evicted, _ := replica.Lines(); evicted != evictedBefore+3 {
		t.Fatalf("first retained line moved from %d to %d, want three more evicted lines", evictedBefore, evicted)
	}
	if got := replica.Text(extent); got != before || got != "10006" {
		t.Fatalf("selection copied %q after eviction, before %q, want %q", got, before, "10006")
	}
}

func TestSearchCoversRetainedScrollbackAndReportsScope(t *testing.T) {
	replica := replicaOf(t, 20, 5, selectionFixture)
	checkSearchScope(t, replica.Search("ST", false), []Match{
		{Start: Point{0, 3}, End: Point{0, 4}},
		{Start: Point{1, 18}, End: Point{1, 19}},
		{Start: Point{5, 12}, End: Point{5, 13}},
	})
	if got := replica.Search("ST", true).Matches; len(got) != 0 {
		t.Fatalf("case-sensitive search found %+v", got)
	}
	checkWrappedSearch(t, replica)
	checkWideSearch(t, replica)
}

func checkSearchScope(t *testing.T, result SearchResult, want []Match) {
	t.Helper()
	if result.FirstLine != 0 || result.EndLine != 7 || result.Limited || len(result.Matches) != len(want) {
		t.Fatalf("result = %+v, want scope 0..7 and %d matches", result, len(want))
	}
	for i, m := range want {
		if result.Matches[i] != m {
			t.Fatalf("match %d = %+v, want %+v", i, result.Matches[i], m)
		}
	}
}

func checkWrappedSearch(t *testing.T, replica *Replica) {
	t.Helper()
	if got := replica.Search("tuv", false).Matches; len(got) != 1 || got[0] != (Match{Start: Point{1, 19}, End: Point{2, 1}}) {
		t.Fatalf("search across the soft wrap = %+v", got)
	}
}

func checkWideSearch(t *testing.T, replica *Replica) {
	t.Helper()
	if got := replica.Search("글 단", false).Matches; len(got) != 1 || got[0] != (Match{Start: Point{5, 2}, End: Point{5, 6}}) {
		t.Fatalf("search over wide cells = %+v", got)
	}
}

func TestSearchStopsAtTheMatchLimit(t *testing.T) {
	stream := ""
	for range MaxSearchMatches + 20 {
		stream += "x\r\n"
	}
	result := replicaOf(t, 20, 5, stream).Search("x", false)
	if !result.Limited || len(result.Matches) != MaxSearchMatches {
		t.Fatalf("matches = %d limited = %v, want %d and limited", len(result.Matches), result.Limited, MaxSearchMatches)
	}
}
