//go:build unix

package advisor_test

import (
	"os"
	"path/filepath"
	"testing"

	"golang.org/x/sys/unix"

	"github.com/gosuda/dalgona/internal/agent/advisor"
)

func TestWatchdog_skipsFifoAndDirectory(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	home := filepath.Join(base, "home")
	if err := os.MkdirAll(filepath.Join(home, "WATCHDOG.md"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := unix.Mkfifo(filepath.Join(base, "WATCHDOG.md"), 0o600); err != nil {
		t.Fatal(err)
	}
	writeWatchdogFile(t, filepath.Join(base, ".dalgona", "WATCHDOG.md"), "regular")
	want := block(filepath.Join(base, ".dalgona", "WATCHDOG.md"), "regular")
	if got := advisor.Watchdog(home, base); got != want {
		t.Fatalf("Watchdog = %q, want %q", got, want)
	}
}
