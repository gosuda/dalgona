package search

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func writeManyHits(t testing.TB, root string, count int) {
	t.Helper()
	for i := range count {
		path := filepath.Join(root, fmt.Sprintf("f%03d.txt", i))
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte("filler line one\nneedle hit line\nfiller line three\n"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
}

func TestGrepParallelMatchesAllFilesInOrder(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeManyHits(t, root, 40)
	result := runMain(t, `{"mode":"grep","pattern":"hit","limit":1000}`)
	if strings.Contains(result.Text, "No matches") {
		t.Fatalf("grep output %q", result.Text)
	}
	rows := strings.Split(strings.TrimSpace(result.Text), "\n")
	if len(rows) != 40 {
		t.Fatalf("rows = %d, want 40: %q", len(rows), result.Text)
	}
	for i := 1; i < len(rows); i++ {
		if rows[i-1] >= rows[i] {
			t.Fatalf("rows out of order: %q before %q", rows[i-1], rows[i])
		}
	}
	if !strings.Contains(rows[0], "f000.txt:2:") {
		t.Fatalf("first row = %q, want f000.txt:2:", rows[0])
	}
}

func BenchmarkGrepManyFiles(b *testing.B) {
	root := b.TempDir()
	writeManyHits(b, root, 200)
	parsed := args{Mode: "grep", Pattern: "hit", Path: ".", Limit: 1000}
	b.ResetTimer()
	for b.Loop() {
		res, resErr := execute(root, parsed, "", freshness{})
		if resErr != nil {
			b.Fatal(resErr)
		}
		if !strings.Contains(res.Text, "f199.txt") {
			b.Fatalf("missing last file in %q", res.Text)
		}
	}
}
