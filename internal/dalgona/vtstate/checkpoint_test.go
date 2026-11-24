package vtstate

import (
	"bytes"
	"errors"
	"fmt"
	"math/rand/v2"
	"slices"
	"strings"
	"testing"
)

type recording struct {
	name    string
	columns int
	rows    int
	stream  string
}

var recordings = []recording{
	{
		name:    "shell prompt with colors and wrap",
		columns: 20,
		rows:    5,
		stream: "\x1b[1;32muser@host\x1b[0m:\x1b[34m~/src\x1b[0m$ ls -la\r\n" +
			"total 48\r\ndrwxr-xr-x  5 user user 4096 Nov 24 a-very-long-file-name-that-wraps.txt\r\n" +
			"\x1b[38;5;208morange\x1b[48;2;10;20;30m truecolor \x1b[4:3mcurly\x1b[58;5;1m\x1b[24m\x1b[0m\r\n$ ",
	},
	{
		name:    "alternate screen editor session",
		columns: 24,
		rows:    6,
		stream: "before\r\n\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H\x1b[7m editor \x1b[27m\x1b[2;1H~\r\n~\r\n\x1b[6;1H\x1b[1m-- INSERT --\x1b[22m" +
			"\x1b[2;3Hhello\x1b[?1h\x1b=\x1b[?2004h\x1b[?1006h\x1b[?1002h\x1b[?25h\x1b[3 q\x1b[?1004h" +
			"\x1b[?1049l\x1b[?1l\x1b>after\r\n",
	},
	{
		name:    "scroll regions and line operations",
		columns: 16,
		rows:    8,
		stream: "\x1b[2;6r\x1b[H1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8\r\n9\x1bM\x1bM\x1b[3L\x1b[2M\x1b[2S\x1b[T" +
			"\x1b[?69h\x1b[3;10s\x1b[4;5Hmargins-left-right\x1b[5;4H\x1b[2@\x1b[3P\x1b[?69l\x1b[r\x1b[8;1Hbottom\x1bD\x1bD",
	},
	{
		name:    "wide CJK cells and pending wrap",
		columns: 9,
		rows:    4,
		stream:  "한국어 텍스트와 漢字が並ぶ行\r\n\x1b[1;8H界x\x1b[2;9H中\x1b[3;1H１２３４５６７８\x1b[3;2Hx\x1b[3;5H\x1b[2X",
	},
	{
		name:    "combining marks emoji and regional indicators",
		columns: 12,
		rows:    4,
		stream:  "e\u0301 a\u0308\u0304 👩\u200d👩\u200d👧 🇰🇷🇯🇵 ✋🏽 नमस्ते\r\n\x1b[31me\x1b[32m\u0301\x1b[0m end",
	},
	{
		name:    "OSC 8 hyperlinks with ids and rejected schemes",
		columns: 30,
		rows:    4,
		stream: "\x1b]8;id=a1;https://example.com/a\x1b\\link-a\x1b]8;;\x1b\\ " +
			"\x1b]8;;mailto:me@example.com\amail\x1b]8;;\a " +
			"\x1b]8;;javascript:alert(1)\x1b\\bad\x1b]8;;\x1b\\ \x1b]8;;https://example.com/b\x1b\\open-link",
	},
	{
		name:    "charsets tabs and saved cursor",
		columns: 30,
		rows:    5,
		stream: "\x1b(0lqqk\x1b(B \x1b)0\x0elqk\x0f \x1b(A#\x1b(B# \x1b*0\x1bNx\x1b[3g\x1b[5G\x1bH\x1b[12G\x1bH\x1b[1G\tA\tB\x1b[Z" +
			"\x1b[2;5H\x1b[1;35m\x1b(0\x1b7\x1b[0m\x1b(B\x1b[4;20Hmoved\x1b8saved\x1b[?6h\x1b[2;4r\x1b[H\x1b7\x1b[?6l\x1b[r\x1b8z" +
			"\x1b[?5W\tT\x1b[0gX\x1b[b",
	},
	{
		name:    "erase operations and background color erase",
		columns: 12,
		rows:    5,
		stream:  "abcdefghijkl\r\nmnopqrstuvwx\r\n0123456789\x1b[44m\x1b[2;4H\x1b[K\x1b[1K\x1b[3;5H\x1b[J\x1b[1;6H\x1b[1J\x1b[0m\x1b[5;1Hlast\x1b[2J\x1b[3J",
	},
	{
		name:    "replies and modes",
		columns: 20,
		rows:    4,
		stream:  "\x1b[c\x1b[>c\x1b[5n\x1b[6n\x1b[?6n\x1b[?2004$p\x1b[20$p\x1b[20h\x1b[?2048h\x1b]10;?\x07\x1b]11;?\x1b\\line\nfeed\x1b[20l\x1b[?2048l\x1b[?7l0123456789012345678901234",
	},
	{
		name:    "unfinished sequences at the end",
		columns: 10,
		rows:    3,
		stream:  "\x1b[1;31mred\x1b]8;;https://example.com/x\x1b\\x\x1b]0;title-that-never-ends",
	},
	{
		name:    "controls executed inside sequences",
		columns: 12,
		rows:    4,
		stream:  "ab\x1b[2\n;5Hc\x1b[3\r1mred\x1b[0m\x1b(\b0q\x1b(B\x1b[?2\t5l x\x1b[?25h\x1b]8;;https://e\x07xample.com\x1b\\y",
	},
}

func feed(t *testing.T, term *Terminal, data string) {
	t.Helper()
	n, err := term.Write([]byte(data))
	if err != nil || n != len(data) {
		t.Fatalf("Write = %d, %v; want %d, nil", n, err, len(data))
	}
}

func checkpoint(t *testing.T, term *Terminal) []byte {
	t.Helper()
	body, err := term.Checkpoint()
	if err != nil {
		t.Fatalf("Checkpoint: %v", err)
	}
	return body
}

type observation struct {
	rows       [][]Cell
	wrapped    []bool
	scrollback [][]Cell
	cursor     CursorState
	modes      InputModes
	pending    string
	alternate  bool
}

func observe(term *Terminal) observation {
	_, rows := term.Size()
	o := observation{cursor: term.Cursor(), modes: term.InputModes(), alternate: term.AlternateScreen()}
	pending, ok := term.PendingCell()
	o.pending = fmt.Sprintf("%q@%d,%d:%v", pending.Cell.Text, pending.Column, pending.Row, ok)
	for y := range rows {
		o.rows = append(o.rows, term.AppendRow(nil, y))
		o.wrapped = append(o.wrapped, term.RowWrapped(y))
	}
	for i := range term.ScrollbackLen() {
		o.scrollback = append(o.scrollback, term.AppendScrollback(nil, i))
	}
	return o
}

func sameObservation(a, b observation) error {
	if a.cursor != b.cursor || a.modes != b.modes || a.pending != b.pending || a.alternate != b.alternate {
		return fmt.Errorf("cursor/modes/pending/alternate differ: %+v %+v %q %v vs %+v %+v %q %v", a.cursor, a.modes, a.pending, a.alternate, b.cursor, b.modes, b.pending, b.alternate)
	}
	if !slices.Equal(a.wrapped, b.wrapped) {
		return fmt.Errorf("soft wraps differ: %v vs %v", a.wrapped, b.wrapped)
	}
	if err := sameRows("screen", a.rows, b.rows); err != nil {
		return err
	}
	return sameRows("scrollback", a.scrollback, b.scrollback)
}

func sameRows(what string, a, b [][]Cell) error {
	if len(a) != len(b) {
		return fmt.Errorf("%s has %d rows vs %d", what, len(a), len(b))
	}
	for y := range a {
		if !slices.EqualFunc(a[y], b[y], Cell.Equal) {
			return fmt.Errorf("%s row %d differs:\n%q\n%q", what, y, rowText(a[y]), rowText(b[y]))
		}
	}
	return nil
}

func rowText(cells []Cell) string {
	var b strings.Builder
	for _, c := range cells {
		b.WriteString(c.Text)
	}
	return b.String()
}

const hiddenStateProbe = "\x1b8P\x1b7\tQ\x1b[bR\x1b[?1049h\x1b8S\x1b[?1049lT\x1b[2;3HU\x1bD\x1bD\x1bD\x1bMV" +
	"\x1b(0q\x1b(Bq\x1b[?6hW\x1b[?6l\x1b[3;1H\x1b[1@\x1b[1PX\x1b[?1047hY\x1b[?1047lZ\x1b[6n\r\n\x1b]8;;https://example.com/p\x1b\\p"

func TestCheckpointRestoreContinuesLikeAnUninterruptedEmulator(t *testing.T) {
	rng := rand.New(rand.NewPCG(1, 2))
	for _, rec := range recordings {
		t.Run(rec.name, func(t *testing.T) {
			stream := rec.stream + hiddenStateProbe
			reference := New(rec.columns, rec.rows)
			feed(t, reference, stream)
			want := checkpoint(t, reference)
			wantView := observe(reference)
			for _, k := range splitPoints(rng, stream, len(rec.stream)) {
				checkResumePoint(t, rec, k, want, wantView)
			}
		})
	}
}

func checkResumePoint(t *testing.T, rec recording, k int, want []byte, wantView observation) {
	t.Helper()
	stream := rec.stream + hiddenStateProbe
	before := New(rec.columns, rec.rows)
	feed(t, before, stream[:k])
	restored, err := Restore(checkpoint(t, before))
	if err != nil {
		t.Fatalf("k=%d: Restore: %v", k, err)
	}
	feed(t, restored, stream[k:])
	if err := sameObservation(wantView, observe(restored)); err != nil {
		t.Fatalf("k=%d (%q|%q): %v", k, stream[max(k-8, 0):k], stream[k:min(k+8, len(stream))], err)
	}
	if got := checkpoint(t, restored); !bytes.Equal(got, want) {
		t.Fatalf("k=%d: final checkpoint differs from the uninterrupted emulator", k)
	}
}

func splitPoints(rng *rand.Rand, stream string, recorded int) []int {
	var points []int
	lastControl := -len(stream)
	for k := 0; k <= recorded; k++ {
		if k > 0 && (stream[k-1] < ' ' || stream[k-1] >= 0x7f) {
			lastControl = k - 1
		}
		if k-lastControl <= 12 || rng.IntN(4) == 0 {
			points = append(points, k)
		}
	}
	for range 10 {
		points = append(points, recorded+rng.IntN(len(stream)-recorded+1))
	}
	return points
}

func TestChunkedWritesMatchOneWrite(t *testing.T) {
	rng := rand.New(rand.NewPCG(3, 4))
	for _, rec := range recordings {
		whole := New(rec.columns, rec.rows)
		feed(t, whole, rec.stream)
		want := checkpoint(t, whole)
		for range 50 {
			chunked := New(rec.columns, rec.rows)
			for rest := rec.stream; rest != ""; {
				n := 1 + rng.IntN(min(len(rest), 7))
				feed(t, chunked, rest[:n])
				rest = rest[n:]
			}
			if !bytes.Equal(checkpoint(t, chunked), want) {
				t.Fatalf("%s: chunked writes produced a different state", rec.name)
			}
		}
	}
}

func TestSplitUTF8SequenceSurvivesCheckpoint(t *testing.T) {
	stream := "a한b"
	reference := New(10, 2)
	feed(t, reference, stream)
	for k := 2; k <= 3; k++ {
		before := New(10, 2)
		feed(t, before, stream[:k])
		restored, err := Restore(checkpoint(t, before))
		if err != nil {
			t.Fatalf("k=%d: %v", k, err)
		}
		feed(t, restored, stream[k:])
		if got := restored.screen().rows[0].cells[1]; got.Text != "한" || got.Width != 2 {
			t.Fatalf("k=%d: cell 1 = %q width %d, want 한 width 2", k, got.Text, got.Width)
		}
		if err := sameObservation(observe(reference), observe(restored)); err != nil {
			t.Fatalf("k=%d: %v", k, err)
		}
	}
}

func TestCheckpointDropsOldestScrollbackToFitAndReportsTheCount(t *testing.T) {
	daemon := New(MaxColumns, 5)
	var stream strings.Builder
	for i := range 1200 {
		fmt.Fprintf(&stream, "%04d %s\r\n", i, strings.Repeat("x", MaxColumns-6))
	}
	feed(t, daemon, stream.String())
	body := checkpoint(t, daemon)
	if len(body) > MaxCheckpointBytes {
		t.Fatalf("checkpoint has %d bytes, more than %d", len(body), MaxCheckpointBytes)
	}
	replica, err := Restore(body)
	if err != nil {
		t.Fatal(err)
	}
	dropped := replica.DroppedScrollbackRows()
	if dropped == 0 || replica.ScrollbackLen()+dropped != daemon.ScrollbackLen() {
		t.Fatalf("replica keeps %d rows and reports %d dropped, daemon keeps %d", replica.ScrollbackLen(), dropped, daemon.ScrollbackLen())
	}
	if got, want := rowText(replica.AppendScrollback(nil, 0)), rowText(daemon.AppendScrollback(nil, dropped)); got != want {
		t.Fatalf("oldest kept row = %.12q, want %.12q", got, want)
	}
	last := replica.ScrollbackLen() - 1
	if got, want := rowText(replica.AppendScrollback(nil, last)), rowText(daemon.AppendScrollback(nil, daemon.ScrollbackLen()-1)); got != want {
		t.Fatalf("newest kept row = %.12q, want %.12q", got, want)
	}
}

func TestRestoreRejectsVersionMismatch(t *testing.T) {
	term := New(10, 3)
	feed(t, term, "hello\x1b[1")
	body := string(checkpoint(t, term))
	cases := map[string]string{
		"schema version": strings.Replace(body, `"version":1`, `"version":2`, 1),
		"engine version": strings.Replace(body, EngineVersion(), EngineVersion()+"-other", 1),
	}
	for name, mutated := range cases {
		if mutated == body {
			t.Fatalf("%s: mutation did not apply", name)
		}
		if _, err := Restore([]byte(mutated)); !errors.Is(err, ErrIncompatible) {
			t.Fatalf("%s: Restore error = %v, want ErrIncompatible", name, err)
		}
	}
}

func TestRestoreRejectsInvalidBodies(t *testing.T) {
	term := New(4, 2)
	feed(t, term, "a中\x1b]8;;https://example.com\x1b\\b")
	body := string(checkpoint(t, term))
	cases := map[string]string{
		"orphan continuation": strings.Replace(body, `{"text":"中","width":2`, `{"text":"中","width":1`, 1),
		"unknown member":      strings.Replace(body, `"cursorVisible":true`, `"cursorVisible":true,"extra":1`, 1),
		"whitespace":          strings.Replace(body, `"version":1`, `"version": 1`, 1),
		"disallowed link":     strings.Replace(body, `https://example.com`, `file:///etc/passwd`, 1),
		"cursor outside":      strings.Replace(body, `"cursor":{"row":0`, `"cursor":{"row":7`, 1),
		"trailing data":       body + "{}",
		"visibility mismatch": strings.Replace(body, `"cursorVisible":true`, `"cursorVisible":false`, 1),
		"duplicate member":    strings.Replace(body, `"cursorVisible":true`, `"cursorVisible":true,"cursorVisible":true`, 1),
	}
	for name, mutated := range cases {
		if mutated == body {
			t.Fatalf("%s: mutation did not apply", name)
		}
		if _, err := Restore([]byte(mutated)); !errors.Is(err, ErrInvalidCheckpoint) {
			t.Fatalf("%s: Restore error = %v, want ErrInvalidCheckpoint", name, err)
		}
	}
}

func FuzzRestore(f *testing.F) {
	for _, rec := range recordings {
		term := New(rec.columns, rec.rows)
		if _, err := term.Write([]byte(rec.stream)); err != nil {
			f.Fatal(err)
		}
		body, err := term.Checkpoint()
		if err != nil {
			f.Fatal(err)
		}
		f.Add(body)
	}
	f.Fuzz(func(t *testing.T, body []byte) {
		restored, err := Restore(body)
		if err != nil {
			return
		}
		again, err := restored.Checkpoint()
		if err != nil || !bytes.Equal(again, body) {
			t.Fatalf("accepted body does not re-encode to itself: %v", err)
		}
		if _, err := restored.Write([]byte("probe\x1b[2J\r\n")); err != nil {
			t.Fatal(err)
		}
	})
}
