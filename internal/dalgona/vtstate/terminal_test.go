package vtstate

import (
	"fmt"
	"strings"
	"testing"
)

func screenText(term *Terminal, y int) string {
	var b strings.Builder
	for _, c := range term.AppendRow(nil, y) {
		b.WriteString(c.Text)
	}
	return b.String()
}

func TestWideCellThatDoesNotFitWrapsToTheNextRow(t *testing.T) {
	term := New(5, 3)
	feed(t, term, "abcd中x")
	if got := screenText(term, 0); got != "abcd " {
		t.Fatalf("row 0 = %q, want %q", got, "abcd ")
	}
	if !term.RowWrapped(0) {
		t.Fatalf("row 0 is not marked soft-wrapped")
	}
	row := term.AppendRow(nil, 1)
	if row[0].Text != "中" || row[0].Width != 2 || !row[1].Continuation() {
		t.Fatalf("row 1 starts with %+v %+v, want 中 width 2 and its continuation", row[0], row[1])
	}
}

func TestCombiningMarkInALaterWriteJoinsItsBase(t *testing.T) {
	term := New(6, 2)
	feed(t, term, "e")
	feed(t, term, "\u0301")
	feed(t, term, "!")
	row := term.AppendRow(nil, 0)
	if row[0].Text != "e\u0301" || row[0].Width != 1 || row[1].Text != "!" {
		t.Fatalf("cells = %q %q, want e+U+0301 then the pending !", row[0].Text, row[1].Text)
	}
	if pending, ok := term.PendingCell(); !ok || pending != (PlacedCell{Cell: Cell{Text: "!", Width: 1}, Column: 1, Row: 0}) {
		t.Fatalf("pending = %+v (%v), want \"!\" width 1 at column 1 row 0", pending, ok)
	}
}

func TestScrollbackEvictsOldestRowsAtTheRowLimit(t *testing.T) {
	term := New(10, 2)
	var stream strings.Builder
	for i := range MaxScrollbackRows + 51 {
		fmt.Fprintf(&stream, "%d\r\n", i)
	}
	feed(t, term, stream.String())
	if got := term.ScrollbackLen(); got != MaxScrollbackRows {
		t.Fatalf("scrollback rows = %d, want %d", got, MaxScrollbackRows)
	}
	if got := rowText(term.AppendScrollback(nil, 0)); got != "50" {
		t.Fatalf("oldest retained row = %q, want %q", got, "50")
	}
}

func TestScrollbackEvictsOldestRowsAtTheByteLimit(t *testing.T) {
	term := New(200, 2)
	line := strings.Repeat("x", 199)
	var stream strings.Builder
	for range 6000 {
		stream.WriteString(line + "\r\n")
	}
	feed(t, term, stream.String())
	want := MaxScrollbackBytes / 200
	if got := term.ScrollbackLen(); got != want {
		t.Fatalf("scrollback rows = %d, want %d rows of 199 bytes plus a line break", got, want)
	}
}

func TestOnlyTheAuthoritativeTerminalRecordsReplies(t *testing.T) {
	query := "ab\x1b[6n\x1b[c"
	authoritative := New(10, 3)
	feed(t, authoritative, query)
	if got, want := string(authoritative.TakeReplies()), "\x1b[1;3R\x1b[?62;1;6;22c"; got != want {
		t.Fatalf("replies = %q, want %q", got, want)
	}
	if got := authoritative.TakeReplies(); got != nil {
		t.Fatalf("second TakeReplies = %q, want nothing", got)
	}
	replica, err := Restore(checkpoint(t, New(10, 3)))
	if err != nil {
		t.Fatal(err)
	}
	feed(t, replica, query)
	if got := replica.TakeReplies(); got != nil {
		t.Fatalf("replica replies = %q, want none", got)
	}
}

func TestHyperlinksOutsideTheAllowlistAreNotAttached(t *testing.T) {
	term := New(20, 2)
	feed(t, term, "\x1b]8;id=x;https://example.com\x1b\\a\x1b]8;;javascript:alert(1)\x1b\\b\x1b]8;;file:///etc\x1b\\c\x1b]8;;mailto:a@b\x1b\\d\x1b]8;;\x1b\\e ")
	row := term.AppendRow(nil, 0)
	want := []*Link{{URI: "https://example.com", ID: "x"}, nil, nil, {URI: "mailto:a@b"}, nil}
	for x, link := range want {
		if !sameLink(row[x].Link, link) {
			t.Fatalf("cell %d (%q) link = %+v, want %+v", x, row[x].Text, row[x].Link, link)
		}
	}
}

func TestResizeAppliesClampedCursorAndFreshTabStops(t *testing.T) {
	term := New(20, 5)
	feed(t, term, "\x1b[5;18H\x1b[3g")
	term.Resize(10, 3)
	c := term.Cursor()
	if c.Column != 9 || c.Row != 2 {
		t.Fatalf("cursor = %d,%d, want 9,2", c.Column, c.Row)
	}
	feed(t, term, "\r\tX\x1b[m")
	if got := screenText(term, 2); got != "        X " {
		t.Fatalf("row 2 = %q, want a tab to column 8", got)
	}
}
