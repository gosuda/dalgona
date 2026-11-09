package daemon

import (
	"runtime"
	"strings"
	"testing"
)

func TestRunCommandSurfacesStderrSoNotLoadedIsDetected(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("sh is unavailable on windows")
	}
	err := runCommand(t.Context(), "sh", "-c", "echo 'Failed to disable unit: Unit file dalgona.service not found.' >&2; exit 1")
	if err == nil || !strings.Contains(err.Error(), "not found") {
		t.Fatalf("runCommand error = %v, want the not-loaded stderr surfaced", err)
	}
	if tolerated := tolerateNotLoaded(err); tolerated != nil {
		t.Fatalf("tolerateNotLoaded(%v) = %v, want nil", err, tolerated)
	}
}

func TestNotLoadedToleranceStillReportsAMissingExecutable(t *testing.T) {
	err := runCommand(t.Context(), "dalgona-missing-binary-xyz")
	if err == nil {
		t.Fatal("missing binary reported no error")
	}
	if tolerated := tolerateNotLoaded(err); tolerated == nil {
		t.Fatalf("tolerateNotLoaded(%v) = nil, want a missing executable reported", err)
	}
}
