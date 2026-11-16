package patch

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func TestResolvePatchPathRejectsVolumeName(t *testing.T) {
	cwd := t.TempDir()
	for _, raw := range []string{"C:x", `C:\x`, "/x", `\x`, `\\server\share`} {
		display, absolute, err := resolvePatchPath(cwd, raw)
		if err == nil {
			t.Fatalf("resolvePatchPath(%q) error = nil, want an outside-workspace refusal", raw)
		}
		want := "patch: " + raw + " is outside the workspace "
		if !strings.HasPrefix(err.Error(), want) {
			t.Errorf("resolvePatchPath(%q) error = %q, want prefix %q", raw, err, want)
		}
		if display != "" || absolute != "" {
			t.Errorf("resolvePatchPath(%q) = %q, %q with an error, want no path to stage", raw, display, absolute)
		}
	}
}

func TestResolvePatchPathAcceptsRelativePath(t *testing.T) {
	cwd := t.TempDir()
	display, absolute, err := resolvePatchPath(cwd, "sub/dir/file.txt")
	if err != nil {
		t.Fatalf("resolvePatchPath error = %v, want nil", err)
	}
	if display != "sub/dir/file.txt" {
		t.Errorf("resolvePatchPath display = %q, want %q", display, "sub/dir/file.txt")
	}
	wantAbsolute := filepath.Join(cwd, filepath.Clean("sub/dir/file.txt"))
	if absolute != wantAbsolute {
		t.Errorf("resolvePatchPath absolute = %q, want %q", absolute, wantAbsolute)
	}
}

func TestRootedAndVolumePathsWriteNothing(t *testing.T) {
	env, cwd := editTestEnv(t)
	outside := filepath.Join(t.TempDir(), "x")
	for _, raw := range []string{"C:x", "/x", outside} {
		input := "[" + snapshot.JSONString(raw) + "#NEW]\nPUT >$:\n+body\n"
		if _, err := runPatch(t, env, HashlineLight, input); err == nil {
			t.Fatalf("patch of %q passed, want an outside-workspace refusal", raw)
		}
	}
	if _, err := os.Stat(outside); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("rooted patch wrote %s: %v", outside, err)
	}
	entries, err := os.ReadDir(cwd)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 0 {
		t.Fatalf("refused patches left %d entries in the workspace", len(entries))
	}
	if _, err := runPatch(t, env, HashlineLight, "[sub/ok.txt#NEW]\nPUT >$:\n+body\n"); err != nil {
		t.Fatalf("relative control patch: %v", err)
	}
	if actual := readFixture(t, cwd, "sub/ok.txt"); actual != "body" {
		t.Fatalf("relative control wrote %q, want %q", actual, "body")
	}
}
