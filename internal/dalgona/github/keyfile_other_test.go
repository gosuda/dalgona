//go:build !windows

package github

import (
	"os"
	"testing"
)

func setTestKeyACL(t *testing.T, path string, perm os.FileMode) {
	t.Helper()
	if err := os.Chmod(path, perm); err != nil {
		t.Fatalf("chmod key file: %v", err)
	}
}
