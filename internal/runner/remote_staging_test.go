package runner

import (
	"path/filepath"
	"strings"
	"testing"
)

func TestStagingPath(t *testing.T) {
	dest := filepath.ToSlash(filepath.Join(t.TempDir(), "target.txt"))
	staged, err := stagingPath(dest)
	if err != nil {
		t.Fatal(err)
	}
	if strings.ContainsRune(staged, '\\') {
		t.Fatalf("staging path %s holds a host separator", staged)
	}
	if dir := dest[:strings.LastIndex(dest, "/")+1]; !strings.HasPrefix(staged, dir) {
		t.Fatalf("staging path %s left destination directory %s", staged, dir)
	}
	base := staged[strings.LastIndex(staged, "/")+1:]
	if len(base) != 33 || !strings.HasPrefix(base, ".dalgona-partial-") {
		t.Fatalf("staging basename %s is %d bytes, want 33", base, len(base))
	}
	other, err := stagingPath(dest)
	if err != nil {
		t.Fatal(err)
	}
	if other == staged {
		t.Fatal("two uploads drew the same staging name")
	}
}
