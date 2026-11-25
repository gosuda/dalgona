//go:build linux || darwin

package runner

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"
)

func replaceDirectoryKeepingTimes(t *testing.T, target string, before os.FileInfo) {
	t.Helper()
	replacement := target + ".replacement"
	if mkdirErr := os.Mkdir(replacement, 0o755); mkdirErr != nil {
		t.Fatal(mkdirErr)
	}
	if removeErr := os.Remove(target); removeErr != nil {
		t.Fatal(removeErr)
	}
	if renameErr := os.Rename(replacement, target); renameErr != nil {
		t.Fatal(renameErr)
	}
	if chtimesErr := os.Chtimes(target, before.ModTime(), before.ModTime()); chtimesErr != nil {
		t.Fatal(chtimesErr)
	}
}

func requireStaleRemoveConflict(t *testing.T, local *Local, root, revision, replaced string) {
	t.Helper()
	_, err := local.Workspace(context.Background(), WorkspaceRequest{
		Op: WorkspaceRemove, Root: root, Path: "target", Recursive: true, Expected: revision,
	})
	stale, ok := errors.AsType[*StaleRevisionError](err)
	if !ok {
		t.Fatalf("remove with replaced directory revision = %v, want a stale revision conflict", err)
	}
	if stale.Current != replaced {
		t.Fatalf("stale revision current = %q, want %q", stale.Current, replaced)
	}
}

func requireDirectoryPresent(t *testing.T, target string) {
	t.Helper()
	if _, err := os.Lstat(target); err != nil {
		t.Fatalf("replacement directory missing after rejected remove: %v", err)
	}
}

func TestDirectoryReplacementChangesRevision(t *testing.T) {
	local, root := workspaceLocal(t)
	target := filepath.Join(root, "target")
	if err := os.Mkdir(target, 0o755); err != nil {
		t.Fatal(err)
	}
	before, err := os.Lstat(target)
	if err != nil {
		t.Fatal(err)
	}
	revision := workspaceRevision(t, local, root, "target")
	replaceDirectoryKeepingTimes(t, target, before)
	replaced := workspaceRevision(t, local, root, "target")
	if replaced == revision {
		t.Fatal("replaced directory kept the old revision")
	}
	requireStaleRemoveConflict(t, local, root, revision, replaced)
	requireDirectoryPresent(t, target)
}
