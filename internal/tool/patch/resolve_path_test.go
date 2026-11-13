package patch

import (
	"fmt"
	"testing"
)

func TestResolvePatchPathRejectsVolumeName(t *testing.T) {
	cwd := t.TempDir()
	for _, raw := range []string{"C:x", `C:\x`} {
		_, _, err := resolvePatchPath(cwd, raw)
		want := fmt.Sprintf("patch: %s is outside the workspace %s", raw, cwd)
		if err == nil || err.Error() != want {
			t.Fatalf("resolvePatchPath(%q) error = %v, want %q", raw, err, want)
		}
	}
}
