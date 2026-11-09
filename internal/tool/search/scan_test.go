package search

import (
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"testing"
)

func scanTempFile(t *testing.T, body string) (grepFile, []int) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "f.txt")
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	entry := fileEntry{absolute: path, display: "f.txt", searchRel: "f.txt"}
	return scanFile(entry, regexp.MustCompile("hit"))
}

func TestScanFileFindsHitsAcrossEndings(t *testing.T) {
	file, matched := scanTempFile(t, "miss\r\nhit\r\nhit\n\nmiss")
	if !slices.Equal(matched, []int{2, 3}) {
		t.Fatalf("matched = %v, want [2 3]", matched)
	}
	if got := string(lineBytes(file.data, file.starts, 2)); got != "hit" {
		t.Fatalf("line 2 = %q, want %q", got, "hit")
	}
	if len(file.starts) != 5 {
		t.Fatalf("line count = %d, want 5", len(file.starts))
	}
}

func TestScanFileTrailingNewlineHasNoGhostLine(t *testing.T) {
	file, matched := scanTempFile(t, "hit\n")
	if !slices.Equal(matched, []int{1}) {
		t.Fatalf("matched = %v, want [1]", matched)
	}
	if len(file.starts) != 1 {
		t.Fatalf("line count = %d, want 1", len(file.starts))
	}
}

func TestScanFileEmptyHasNoLines(t *testing.T) {
	file, matched := scanTempFile(t, "")
	if len(matched) != 0 || len(file.starts) != 0 {
		t.Fatalf("matched = %v starts = %v, want empty", matched, file.starts)
	}
}

func BenchmarkScanFileLarge(b *testing.B) {
	dir := b.TempDir()
	path := filepath.Join(dir, "large.txt")
	body := make([]byte, 0, 8<<20)
	for i := range 200000 {
		body = append(body, "line content with filler words to widen rows"...)
		if i%50 == 0 {
			body = append(body, " hit"...)
		}
		body = append(body, '\n')
	}
	if err := os.WriteFile(path, body, 0o644); err != nil {
		b.Fatal(err)
	}
	entry := fileEntry{absolute: path, display: "large.txt", searchRel: "large.txt"}
	matcher := regexp.MustCompile("hit")
	b.ResetTimer()
	for b.Loop() {
		file, matched := scanFile(entry, matcher)
		if len(matched) != 4000 || len(file.starts) != 200000 {
			b.Fatalf("matched = %d starts = %d", len(matched), len(file.starts))
		}
	}
}

func largeBody() []byte {
	body := make([]byte, 0, 8<<20)
	for i := range 200000 {
		body = append(body, "line content with filler words to widen rows"...)
		if i%50 == 0 {
			body = append(body, " hit"...)
		}
		body = append(body, '\n')
	}
	return body
}

func BenchmarkSplitWholeCopy(b *testing.B) {
	data := largeBody()
	b.ResetTimer()
	for b.Loop() {
		lines := strings.Split(string(data), "\n")
		if len(lines) != 200001 {
			b.Fatalf("parts = %d", len(lines))
		}
	}
}

func BenchmarkSplitStarts(b *testing.B) {
	data := largeBody()
	b.ResetTimer()
	for b.Loop() {
		starts := splitStarts(data)
		if len(starts) != 200000 {
			b.Fatalf("starts = %d", len(starts))
		}
	}
}
