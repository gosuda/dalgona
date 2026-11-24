package vtstate

import (
	"runtime"
	"strings"
	"testing"
)

func TestPendingC1ControlCheckpointRoundTrips(t *testing.T) {
	term := New(10, 3)
	feed(t, term, "ab\xc2\x85")
	body := checkpoint(t, term)
	restored, err := Restore(body)
	if err != nil {
		t.Fatalf("Restore after a pending C1 control = %v, want success", err)
	}
	if err := sameObservation(observe(term), observe(restored)); err != nil {
		t.Fatalf("restored terminal differs: %v", err)
	}
	if got := screenText(term, 0); got != "ab        " {
		t.Fatalf("row 0 = %q, want the C1 control ignored", got)
	}
}

func TestScrollbackBudgetCountsRetainedLinkBytes(t *testing.T) {
	var kept scrollback
	uri := "https://" + strings.Repeat("e", 2048) + ".example/"
	link := &Link{URI: uri}
	for range MaxScrollbackRows {
		kept.push(row{cells: []Cell{{Text: "x", Width: 1, Link: link}}})
	}
	if got := kept.len(); got >= 2000 {
		t.Fatalf("scrollback holds %d rows, want eviction under the byte budget", got)
	}
	if kept.bytes > MaxScrollbackBytes {
		t.Fatalf("scrollback holds %d bytes, want at most %d", kept.bytes, MaxScrollbackBytes)
	}
}

func TestCheckpointBoundsTransientEncodingMemory(t *testing.T) {
	term := New(240, 5)
	line := strings.Repeat("ab", 120)
	for range MaxScrollbackRows {
		feed(t, term, line+"\r\n")
	}
	if term.ScrollbackLen() == 0 {
		t.Fatal("want retained scrollback before the checkpoint")
	}
	runtime.GC()
	var before runtime.MemStats
	runtime.ReadMemStats(&before)
	body := checkpoint(t, term)
	var after runtime.MemStats
	runtime.ReadMemStats(&after)
	if len(body) > MaxCheckpointBytes {
		t.Fatalf("checkpoint holds %d bytes, want at most %d", len(body), MaxCheckpointBytes)
	}
	budget := uint64(100 << 20)
	if grown := after.TotalAlloc - before.TotalAlloc; grown > budget {
		t.Fatalf("checkpoint allocated %d bytes, want bounded transient encoding", grown)
	}
	assertTruncatedRoundTrip(t, term, body)
}

func assertTruncatedRoundTrip(t *testing.T, term *Terminal, body []byte) {
	t.Helper()
	restored, err := Restore(body)
	if err != nil {
		t.Fatalf("Restore = %v, want success", err)
	}
	if term.ScrollbackLen() <= restored.ScrollbackLen() {
		t.Fatalf("checkpoint kept %d of %d rows, want oldest rows dropped", restored.ScrollbackLen(), term.ScrollbackLen())
	}
	again, err := restored.Checkpoint()
	if err != nil {
		t.Fatalf("second Checkpoint = %v, want success", err)
	}
	replayed, err := Restore(again)
	if err != nil {
		t.Fatalf("second Restore = %v, want success", err)
	}
	if err := sameObservation(observe(restored), observe(replayed)); err != nil {
		t.Fatalf("restored terminal differs: %v", err)
	}
}

func TestPendingCellAtScrollBottomProjectsSameRow(t *testing.T) {
	term := New(5, 3)
	feed(t, term, "abcdefghijklmnop")
	pending, ok := term.PendingCell()
	if !ok || pending.Column != 0 || pending.Row != 2 || pending.Cell.Text != "p" {
		t.Fatalf("pending = %+v (%v), want \"p\" at column 0 row 2", pending, ok)
	}
	if got := term.AppendRow(nil, 2); got[0].Text != "p" {
		t.Fatalf("rendered row starts with %q, want the pending glyph", got[0].Text)
	}
	feed(t, term, "!")
	if got := screenText(term, 2); got != "p!   " {
		t.Fatalf("committed row = %q, want the projected scroll position", got)
	}
}
