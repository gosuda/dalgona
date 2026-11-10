package prompt_test

import (
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/agent/prompt"
)

func TestInstructionsWalkAndFallback(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	home := filepath.Join(base, "home")
	repo := filepath.Join(base, "repo")
	cwd := filepath.Join(repo, "sub")
	writeTestFile(t, filepath.Join(home, "AGENTS.md"), "HOME")
	if err := os.MkdirAll(filepath.Join(repo, ".git"), 0o755); err != nil {
		t.Fatalf("create git marker: %v", err)
	}
	writeTestFile(t, filepath.Join(repo, "AGENTS.md"), "ROOT")
	writeTestFile(t, filepath.Join(repo, "CLAUDE.md"), "MUST NOT WIN")
	writeTestFile(t, filepath.Join(cwd, "CLAUDE.md"), "SUB")
	got := prompt.Instructions(home, cwd)
	assertOrder(t, got, ">HOME</instructions>", ">ROOT</instructions>", ">SUB</instructions>")
	if strings.Contains(got, "MUST NOT WIN") {
		t.Fatalf("CLAUDE.md replaced an existing AGENTS.md: %s", got)
	}
	if count := strings.Count(got, "<instructions path="); count != 3 {
		t.Fatalf("instruction count = %d, want 3: %s", count, got)
	}
}

func TestInstructionsDoesNotDuplicateHome(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if err := os.Mkdir(filepath.Join(home, ".git"), 0o755); err != nil {
		t.Fatalf("create git marker: %v", err)
	}
	writeTestFile(t, filepath.Join(home, "AGENTS.md"), "HOME")
	got := prompt.Instructions(home, home)
	if count := strings.Count(got, "<instructions path="); count != 1 {
		t.Fatalf("home instruction count = %d, want 1: %s", count, got)
	}
}

func TestInstructionsHomeClaudeFallback(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if err := os.Mkdir(filepath.Join(home, ".git"), 0o755); err != nil {
		t.Fatalf("create git marker: %v", err)
	}
	writeTestFile(t, filepath.Join(home, "CLAUDE.md"), "HOME FALLBACK")
	got := prompt.Instructions(home, home)
	if count := strings.Count(got, "<instructions path="); count != 1 || !strings.Contains(got, "HOME FALLBACK") {
		t.Fatalf("home fallback instructions = %s", got)
	}
}

func TestInstructionsCap(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	home := filepath.Join(base, "home")
	cwd := filepath.Join(base, "cwd")
	writeTestFile(t, filepath.Join(home, "AGENTS.md"), strings.Repeat("a", 32*1024)+"OUTSIDE CAP")
	got := prompt.Instructions(home, cwd)
	if !strings.Contains(got, "[instructions truncated at 32 KiB]") {
		t.Fatalf("missing truncation line: %s", got[len(got)-100:])
	}
	if strings.Contains(got, "OUTSIDE CAP") {
		t.Fatal("content beyond 32 KiB reached the prompt")
	}
}

func TestInstructionsCapPreservesUTF8(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	home := filepath.Join(base, "home")
	cwd := filepath.Join(base, "cwd")
	writeTestFile(t, filepath.Join(home, "AGENTS.md"), strings.Repeat("a", 32*1024-1)+"éOUTSIDE CAP")
	got := prompt.Instructions(home, cwd)
	if !utf8.ValidString(got) {
		t.Fatal("instruction cap split a UTF-8 rune")
	}
	if strings.Contains(got, "é") || !strings.Contains(got, "[instructions truncated at 32 KiB]") {
		t.Fatalf("truncation did not drop the partial rune cleanly")
	}
}

func TestNestedAppendsEachFileOnce(t *testing.T) {
	t.Parallel()
	cwd := t.TempDir()
	writeTestFile(t, filepath.Join(cwd, "AGENTS.md"), "ROOT MUST NOT APPEAR")
	subAgents := filepath.Join(cwd, "sub", "AGENTS.md")
	subClaude := filepath.Join(cwd, "sub", "CLAUDE.md")
	deepAgents := filepath.Join(cwd, "sub", "deep", "AGENTS.md")
	writeTestFile(t, subAgents, "SUB AGENTS")
	writeTestFile(t, subClaude, "SUB CLAUDE")
	writeTestFile(t, deepAgents, "DEEP AGENTS")
	readPath := filepath.Join(cwd, "sub", "deep", "x.go")
	writeTestFile(t, readPath, "package x")
	seen := map[string]bool{}
	got, newlySeen := prompt.Nested(cwd, readPath, seen)
	assertOrder(t, got, "SUB AGENTS", "SUB CLAUDE", "DEEP AGENTS")
	if strings.Contains(got, "ROOT MUST NOT APPEAR") {
		t.Fatalf("cwd instruction was injected by Nested: %s", got)
	}
	canonicalCwd, err := filepath.EvalSymlinks(cwd)
	if err != nil {
		t.Fatalf("resolve temp dir: %v", err)
	}
	wantPaths := []string{filepath.Join(canonicalCwd, "sub", "AGENTS.md"), filepath.Join(canonicalCwd, "sub", "CLAUDE.md"), filepath.Join(canonicalCwd, "sub", "deep", "AGENTS.md")}
	if !slices.Equal(newlySeen, wantPaths) {
		t.Fatalf("newlySeen = %q, want %q", newlySeen, wantPaths)
	}
	again, againPaths := prompt.Nested(cwd, readPath, seen)
	if again != "" || len(againPaths) != 0 {
		t.Fatalf("second Nested returned text %q and paths %q", again, againPaths)
	}
}

func TestNestedCapAndContainment(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	cwd := filepath.Join(base, "cwd")
	readPath := filepath.Join(cwd, "sub", "x.go")
	writeTestFile(t, filepath.Join(cwd, "sub", "AGENTS.md"), strings.Repeat("n", 8*1024)+"OUTSIDE CAP")
	writeTestFile(t, readPath, "package x")
	got, _ := prompt.Nested(cwd, readPath, map[string]bool{})
	if !strings.Contains(got, "[instructions truncated at 8 KiB]") || strings.Contains(got, "OUTSIDE CAP") {
		t.Fatalf("nested cap was not enforced")
	}
	outside := filepath.Join(base, "outside.go")
	writeTestFile(t, outside, "package outside")
	if got, paths := prompt.Nested(cwd, outside, map[string]bool{}); got != "" || len(paths) != 0 {
		t.Fatalf("outside read returned instructions %q and paths %q", got, paths)
	}
}

func writeTestFile(t *testing.T, path, text string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("create parent for %s: %v", path, err)
	}
	if err := os.WriteFile(path, []byte(text), 0o644); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
}

func symlinkOrSkip(t *testing.T, target, link string) {
	t.Helper()
	if err := os.Symlink(target, link); err != nil {
		t.Skipf("symlink creation requires additional privileges: %v", err)
	}
}

func TestInstructionsIgnoreOutsideSymlink(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	home := filepath.Join(base, "home")
	repo := filepath.Join(base, "repo")
	if err := os.MkdirAll(filepath.Join(repo, ".git"), 0o755); err != nil {
		t.Fatalf("create git marker: %v", err)
	}
	writeTestFile(t, filepath.Join(home, "AGENTS.md"), "HOME")
	outside := filepath.Join(base, "outside")
	writeTestFile(t, filepath.Join(outside, "secret.md"), "TOP-SECRET-OUTSIDE")
	escape := filepath.Join(repo, "AGENTS.md")
	symlinkOrSkip(t, filepath.Join(outside, "secret.md"), escape)
	cwd := filepath.Join(repo, "sub")
	if err := os.MkdirAll(cwd, 0o755); err != nil {
		t.Fatal(err)
	}
	if got := prompt.Instructions(home, cwd); strings.Contains(got, "TOP-SECRET-OUTSIDE") {
		t.Fatalf("outside symlink reached the instructions: %s", got)
	}
	inner := filepath.Join(repo, "inner.md")
	writeTestFile(t, inner, "INNER-OK")
	if err := os.Remove(escape); err != nil {
		t.Fatal(err)
	}
	symlinkOrSkip(t, inner, escape)
	if got := prompt.Instructions(home, cwd); !strings.Contains(got, "INNER-OK") {
		t.Fatalf("inside link did not load: %s", got)
	}
	deep := filepath.Join(cwd, "deep", "x.go")
	writeTestFile(t, deep, "package x")
	if err := os.Remove(escape); err != nil {
		t.Fatal(err)
	}
	symlinkOrSkip(t, filepath.Join(outside, "secret.md"), filepath.Join(cwd, "deep", "AGENTS.md"))
	if got, _ := prompt.Nested(cwd, deep, map[string]bool{}); strings.Contains(got, "TOP-SECRET-OUTSIDE") {
		t.Fatalf("outside nested symlink reached the instructions: %s", got)
	}
}
