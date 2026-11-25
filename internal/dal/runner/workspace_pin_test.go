package runner

import (
	"context"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

func writePinnedFile(t *testing.T, dir, name, data string) {
	t.Helper()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, name), []byte(data), 0o644); err != nil {
		t.Fatal(err)
	}
}

func swapPinnedWithSymlink(t *testing.T, approved, target string) bool {
	t.Helper()
	kept := approved + ".kept"
	if err := os.Rename(approved, kept); err != nil {
		if runtime.GOOS == "windows" {
			if _, statErr := os.Stat(approved); statErr != nil {
				t.Fatalf("pinned rename refused but directory missing: %v", statErr)
			}
			if _, statErr := os.Lstat(kept); !os.IsNotExist(statErr) {
				t.Fatalf("pinned rename left %s behind: %v", kept, statErr)
			}
			t.Skipf("pinned directory could not be swapped on Windows (rename refused, pin enforced): %v", err)
			return false
		}
		t.Fatal(err)
	}
	if err := os.Symlink(target, approved); err != nil {
		_ = os.Rename(kept, approved)
		t.Skipf("symlink unavailable: %v", err)
	}
	return true
}

func TestLocalPinnedSurvivesSymlinkSwap(t *testing.T) {
	base := t.TempDir()
	approved := filepath.Join(base, "approved")
	writePinnedFile(t, approved, "data.txt", "original")
	target := filepath.Join(base, "target")
	writePinnedFile(t, target, "data.txt", "evil")
	local, err := NewLocal(filepath.Join(base, "runs"))
	if err != nil {
		t.Fatal(err)
	}
	session, err := local.OpenWorkspace(t.Context(), approved)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = session.Close() }()
	if got := session.DisplayRoot(); got == "" {
		t.Fatal("empty display root")
	}
	if !swapPinnedWithSymlink(t, approved, target) {
		return
	}
	result, err := session.Workspace(t.Context(), WorkspaceRequest{Op: WorkspaceRead, Path: "data.txt", Limit: WorkspaceChunkLimit})
	if err != nil {
		t.Fatalf("pinned read failed: %v", err)
	}
	if string(result.Data) != "original" {
		t.Fatalf("pinned read = %q, want original", result.Data)
	}
	if err := session.VerifyExec(t.Context()); err == nil {
		t.Fatal("VerifyExec succeeded after symlink swap, want failure")
	}
}

func TestRemotePinnedSurvivesSymlinkSwap(t *testing.T) {
	base := t.TempDir()
	approved := filepath.Join(base, "approved")
	writePinnedFile(t, approved, "data.txt", "original")
	target := filepath.Join(base, "target")
	writePinnedFile(t, target, "data.txt", "evil")
	box, err := NewLocal(filepath.Join(base, "runs"))
	if err != nil {
		t.Fatal(err)
	}
	serverEnd, clientEnd := net.Pipe()
	go func() { _ = Serve(t.Context(), serverEnd, box) }()
	remote, err := NewRemote(t.Context(), "box-test", clientEnd)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = remote.Close() }()
	session, err := remote.OpenWorkspace(t.Context(), approved)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = session.Close() }()
	if !swapPinnedWithSymlink(t, approved, target) {
		return
	}
	result, err := session.Workspace(t.Context(), WorkspaceRequest{Op: WorkspaceRead, Path: "data.txt", Limit: WorkspaceChunkLimit})
	if err != nil {
		t.Fatalf("pinned remote read failed: %v", err)
	}
	if string(result.Data) != "original" {
		t.Fatalf("pinned remote read = %q, want original", result.Data)
	}
	if err := session.VerifyExec(t.Context()); err == nil {
		t.Fatal("remote VerifyExec succeeded after swap, want failure")
	}
}

func TestLocalOpenWorkspaceRejectsMissing(t *testing.T) {
	local, err := NewLocal(filepath.Join(t.TempDir(), "runs"))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := local.OpenWorkspace(t.Context(), filepath.Join(t.TempDir(), "absent")); err == nil {
		t.Fatal("open of absent directory succeeded")
	}
}

func TestLocalPinnedCloseIdempotent(t *testing.T) {
	dir := t.TempDir()
	approved := filepath.Join(dir, "approved")
	writePinnedFile(t, approved, "data.txt", "original")
	local, err := NewLocal(filepath.Join(dir, "runs"))
	if err != nil {
		t.Fatal(err)
	}
	session, err := local.OpenWorkspace(t.Context(), approved)
	if err != nil {
		t.Fatal(err)
	}
	if err := session.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := session.Workspace(context.Background(), WorkspaceRequest{Op: WorkspaceStat, Path: ""}); err == nil {
		t.Fatal("operation on closed handle succeeded")
	}
}
