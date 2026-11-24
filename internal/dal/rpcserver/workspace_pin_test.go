package rpcserver

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func writePinFile(t *testing.T, dir, name, data string) {
	t.Helper()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, name), []byte(data), 0o644); err != nil {
		t.Fatal(err)
	}
}

func swapApprovedWithSymlink(t *testing.T, approved, target string) {
	t.Helper()
	kept := approved + ".kept"
	if err := os.Rename(approved, kept); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(target, approved); err != nil {
		_ = os.Rename(kept, approved)
		t.Skipf("symlink unavailable: %v", err)
	}
}

func readPinnedText(t *testing.T, w *workspaceWindow, id string) string {
	t.Helper()
	var chunk workspaceChunk
	w.must("dalgona/workspace/read", map[string]any{"workspaceId": id, "path": "data.txt", "offset": "0", "limit": 64}, &chunk)
	return string(chunk.DataBase64)
}

func TestWorkspacePinnedAfterOpenIgnoresSymlinkSwap(t *testing.T) {
	eachWorkspaceRunner(t, func(t *testing.T, r *rig) {
		base := t.TempDir()
		approved := filepath.Join(base, "approved")
		writePinFile(t, approved, "data.txt", "original")
		target := filepath.Join(base, "target")
		writePinFile(t, target, "data.txt", "evil")
		threadID := startWorkspaceThread(t, r, approved)
		w := openWorkspaceWindow(t, r)
		opened := w.open(threadID, "read")
		swapApprovedWithSymlink(t, approved, target)
		got := readPinnedText(t, w, opened.WorkspaceID)
		if got == "evil" {
			t.Fatal("pinned read followed symlink to evil target")
		}
		if got != "original" {
			t.Fatalf("pinned read = %q, want original", got)
		}
	})
}

func TestWorkspacePinnedDuringPromptIgnoresSymlinkSwap(t *testing.T) {
	eachWorkspaceRunner(t, func(t *testing.T, r *rig) {
		base := t.TempDir()
		approved := filepath.Join(base, "approved")
		writePinFile(t, approved, "data.txt", "original")
		target := filepath.Join(base, "target")
		writePinFile(t, target, "data.txt", "evil")
		threadID := startWorkspaceThread(t, r, approved)
		w := openWorkspaceWindow(t, r)
		opened := openWithSwapDuringPrompt(t, w, threadID, approved, target)
		got := readPinnedText(t, w, opened.WorkspaceID)
		if got == "evil" {
			t.Fatal("prompt-pinned read followed symlink to evil target")
		}
		if got != "original" {
			t.Fatalf("prompt-pinned read = %q, want original", got)
		}
	})
}

func openWithSwapDuringPrompt(t *testing.T, w *workspaceWindow, threadID, approved, target string) workspaceOpened {
	t.Helper()
	openedCh, errCh := startPromptedOpen(t, w, threadID)
	awaitPrompt(t, w, errCh)
	swapApprovedWithSymlink(t, approved, target)
	w.decisions <- workspaceAllowOnce
	return awaitOpened(t, openedCh, errCh)
}

func startPromptedOpen(t *testing.T, w *workspaceWindow, threadID string) (chan workspaceOpened, chan error) {
	t.Helper()
	openedCh := make(chan workspaceOpened, 1)
	errCh := make(chan error, 1)
	go func() {
		var opened workspaceOpened
		err := w.call("dalgona/workspace/open", map[string]any{"threadId": threadID, "root": "cwd", "rights": []string{"read"}, "operationId": rpc.NewOperationID()}, &opened)
		if err != nil {
			errCh <- err
			return
		}
		openedCh <- opened
	}()
	return openedCh, errCh
}

func awaitPrompt(t *testing.T, w *workspaceWindow, errCh chan error) {
	t.Helper()
	select {
	case <-w.asked:
	case err := <-errCh:
		t.Fatalf("open failed before prompt: %v", err)
	case <-time.After(waitLimit):
		t.Fatal("permission prompt did not arrive")
	}
}

func awaitOpened(t *testing.T, openedCh chan workspaceOpened, errCh chan error) workspaceOpened {
	t.Helper()
	select {
	case opened := <-openedCh:
		return opened
	case err := <-errCh:
		t.Fatalf("open failed after prompt: %v", err)
	case <-time.After(waitLimit):
		t.Fatal("open did not complete after decision")
	}
	return workspaceOpened{}
}

func TestWorkspacePinnedExecFailsClosedAfterSwap(t *testing.T) {
	eachWorkspaceRunner(t, func(t *testing.T, r *rig) {
		base := t.TempDir()
		approved := filepath.Join(base, "approved")
		writePinFile(t, approved, "data.txt", "original")
		target := filepath.Join(base, "target")
		writePinFile(t, target, "data.txt", "evil")
		threadID := startWorkspaceThread(t, r, approved)
		w := openWorkspaceWindow(t, r)
		opened := w.open(threadID, "read", "terminal")
		swapApprovedWithSymlink(t, approved, target)
		err := w.call("dalgona/workspace/exec", map[string]any{"workspaceId": opened.WorkspaceID, "text": "cat data.txt", "operationId": rpc.NewOperationID()}, nil)
		if err == nil {
			t.Fatal("exec after symlink swap succeeded, want fail closed")
		}
	})
}
