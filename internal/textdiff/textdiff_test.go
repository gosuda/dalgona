package textdiff

import (
	"strings"
	"testing"
)

func TestUnifiedCountsAddedAndRemovedLines(t *testing.T) {
	text, stats, err := Unified("a/f.txt", "b/f.txt", []byte("a\nb\n"), []byte("a\nc\nd\n"), 3)
	if err != nil {
		t.Fatalf("Unified returned error: %v", err)
	}
	if stats != (Stats{Added: 2, Removed: 1}) {
		t.Fatalf("stats = %+v, want {Added:2 Removed:1}", stats)
	}
	for _, want := range []string{"-b", "+c", "+d"} {
		if !strings.Contains(text, want) {
			t.Fatalf("diff text %q lacks %q", text, want)
		}
	}
}

func TestUnifiedCountsLinesThatStartWithDashes(t *testing.T) {
	text, stats, err := Unified("a/f.txt", "b/f.txt", []byte("-- x\n"), []byte("-- y\n"), 3)
	if err != nil {
		t.Fatalf("Unified returned error: %v", err)
	}
	if stats != (Stats{Added: 1, Removed: 1}) {
		t.Fatalf("stats = %+v, want {Added:1 Removed:1} without counting the header lines", stats)
	}
	if !strings.HasPrefix(text, "--- a/f.txt\n+++ b/f.txt\n") {
		t.Fatalf("diff text %q lacks the two header lines", text)
	}
}

func TestUnifiedDoesNotCountTheNoNewlineMarker(t *testing.T) {
	text, stats, err := Unified("a/f.txt", "b/f.txt", []byte("a"), []byte("b"), 3)
	if err != nil {
		t.Fatalf("Unified returned error: %v", err)
	}
	if stats != (Stats{Added: 1, Removed: 1}) {
		t.Fatalf("stats = %+v, want {Added:1 Removed:1} without counting the marker line", stats)
	}
	if !strings.Contains(text, `\ No newline at end of file`) {
		t.Fatalf("diff text %q lacks the missing-newline marker", text)
	}
}

func TestUnifiedEqualInputsReturnEmpty(t *testing.T) {
	text, stats, err := Unified("a/f.txt", "b/f.txt", []byte("a\nb\n"), []byte("a\nb\n"), 3)
	if err != nil {
		t.Fatalf("Unified returned error: %v", err)
	}
	if text != "" {
		t.Fatalf("text = %q, want empty", text)
	}
	if stats != (Stats{}) {
		t.Fatalf("stats = %+v, want zero Stats", stats)
	}
}

func TestUnifiedContextZeroHasNoContextLines(t *testing.T) {
	text, stats, err := Unified("a/f.txt", "b/f.txt", []byte("a\nb\nc\nd\ne\n"), []byte("a\nb\nc\nX\ne\n"), 0)
	if err != nil {
		t.Fatalf("Unified returned error: %v", err)
	}
	if stats != (Stats{Added: 1, Removed: 1}) {
		t.Fatalf("stats = %+v, want {Added:1 Removed:1}", stats)
	}
	for _, line := range strings.Split(text, "\n")[2:] {
		if strings.HasPrefix(line, " ") {
			t.Fatalf("context-0 diff text %q holds an unchanged line after the headers", text)
		}
	}
}
