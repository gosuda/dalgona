package search

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func writeScopeFixture(t *testing.T) string {
	t.Helper()
	cwd := t.TempDir()
	writeTreeFile(t, filepath.Join(cwd, "sub", ".gitignore"), "gen/\n")
	writeTreeFile(t, filepath.Join(cwd, "sub", "gen", "x.txt"), "needle\n")
	writeTreeFile(t, filepath.Join(cwd, "sub", "keep.txt"), "needle\n")
	writeTreeFile(t, filepath.Join(cwd, "sub", "deep", ".gitignore"), "more/\n")
	writeTreeFile(t, filepath.Join(cwd, "sub", "deep", "more", "y.txt"), "needle\n")
	writeTreeFile(t, filepath.Join(cwd, "sub", "deep", "real.txt"), "needle\n")
	return cwd
}

func writeTreeFile(t *testing.T, path, body string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

func grepHits(t *testing.T, cwd, scope string) string {
	t.Helper()
	result, err := execute(cwd, args{Mode: "grep", Pattern: "needle", Path: scope, Limit: 100})
	if err != nil {
		t.Fatalf("grep failed: %v", err)
	}
	return result.Text
}

func TestScopedGrepHonorsScopeGitignore(t *testing.T) {
	cwd := writeScopeFixture(t)
	if hits := grepHits(t, cwd, ""); strings.Contains(hits, "gen/x.txt") || strings.Contains(hits, "y.txt") {
		t.Fatalf("workspace grep returned ignored files: %s", hits)
	}
	if hits := grepHits(t, cwd, "sub"); strings.Contains(hits, "gen/x.txt") || strings.Contains(hits, "y.txt") {
		t.Fatalf("scoped grep returned files ignored by the scope's gitignores: %s", hits)
	}
	if hits := grepHits(t, cwd, "sub/deep"); strings.Contains(hits, "y.txt") {
		t.Fatalf("scoped grep returned files ignored by an ancestor gitignore: %s", hits)
	}
}

func TestScopedGrepStillReturnsKeptFiles(t *testing.T) {
	cwd := writeScopeFixture(t)
	hits := grepHits(t, cwd, "sub/deep")
	if !strings.Contains(hits, "real.txt") {
		t.Fatalf("scoped grep lost kept files: %s", hits)
	}
}
