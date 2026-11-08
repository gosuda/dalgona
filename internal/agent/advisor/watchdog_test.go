package advisor_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/advisor"
)

func writeWatchdogFile(t *testing.T, path, body string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
}

func block(path, body string) string {
	return `<watchdog path="` + path + `">` + body + `</watchdog>`
}

func TestWatchdog_orderFromHomeThroughRepositoryToCwd(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	home := filepath.Join(base, "home")
	root := filepath.Join(base, "outer", "repo")
	cwd := filepath.Join(root, "a", "b")
	files := map[string]string{
		filepath.Join(home, "WATCHDOG.md"):                 "home",
		filepath.Join(base, "outer", "WATCHDOG.md"):        "outside repo",
		filepath.Join(root, "WATCHDOG.md"):                 "root",
		filepath.Join(root, ".dalgona", "WATCHDOG.md"):     "root dalgona",
		filepath.Join(root, "a", "WATCHDOG.md"):            "a",
		filepath.Join(cwd, ".dalgona", "WATCHDOG.md"):      "b dalgona",
		filepath.Join(root, "a", "sibling", "WATCHDOG.md"): "sibling",
	}
	for path, body := range files {
		writeWatchdogFile(t, path, body)
	}
	if err := os.Mkdir(filepath.Join(root, ".git"), 0o755); err != nil {
		t.Fatal(err)
	}
	want := strings.Join([]string{
		block(filepath.Join(home, "WATCHDOG.md"), "home"),
		block(filepath.Join(root, "WATCHDOG.md"), "root"),
		block(filepath.Join(root, ".dalgona", "WATCHDOG.md"), "root dalgona"),
		block(filepath.Join(root, "a", "WATCHDOG.md"), "a"),
		block(filepath.Join(cwd, ".dalgona", "WATCHDOG.md"), "b dalgona"),
	}, "\n\n")
	if got := advisor.Watchdog(home, cwd); got != want {
		t.Fatalf("Watchdog =\n%s\nwant\n%s", got, want)
	}
}

func TestWatchdog_gitFileMarksWorktreeRoot(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	root := filepath.Join(base, "worktree")
	cwd := filepath.Join(root, "pkg")
	writeWatchdogFile(t, filepath.Join(root, ".git"), "gitdir: /elsewhere\n")
	writeWatchdogFile(t, filepath.Join(root, "WATCHDOG.md"), "root")
	writeWatchdogFile(t, filepath.Join(cwd, "WATCHDOG.md"), "pkg")
	want := block(filepath.Join(root, "WATCHDOG.md"), "root") + "\n\n" + block(filepath.Join(cwd, "WATCHDOG.md"), "pkg")
	if got := advisor.Watchdog(filepath.Join(base, "no-home"), cwd); got != want {
		t.Fatalf("Watchdog = %q, want %q", got, want)
	}
}

func TestWatchdog_withoutRepositoryReadsOnlyCwd(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	cwd := filepath.Join(base, "project", "sub")
	writeWatchdogFile(t, filepath.Join(base, "project", "WATCHDOG.md"), "parent")
	writeWatchdogFile(t, filepath.Join(cwd, "WATCHDOG.md"), "cwd")
	want := block(filepath.Join(cwd, "WATCHDOG.md"), "cwd")
	if got := advisor.Watchdog(filepath.Join(base, "no-home"), cwd); got != want {
		t.Fatalf("Watchdog = %q, want %q", got, want)
	}
}

func TestWatchdog_homeInsideProjectAppearsOnceAndPathIsEscaped(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	cwd := filepath.Join(base, `a&"b`)
	home := filepath.Join(cwd, ".dalgona")
	writeWatchdogFile(t, filepath.Join(home, "WATCHDOG.md"), "<raw & kept>")
	escaped := filepath.Join(base, "a&amp;&#34;b", ".dalgona", "WATCHDOG.md")
	want := `<watchdog path="` + escaped + `"><raw & kept></watchdog>`
	if got := advisor.Watchdog(home, cwd); got != want {
		t.Fatalf("Watchdog = %q, want %q", got, want)
	}
}

func TestWatchdog_noFiles(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	if got := advisor.Watchdog(filepath.Join(base, "home"), base); got != "" {
		t.Fatalf("Watchdog = %q, want empty", got)
	}
}
