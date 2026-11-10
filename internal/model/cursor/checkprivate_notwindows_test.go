//go:build !windows

package cursor_test

import (
	"os"
	"testing"
)

func checkPrivateFile(t *testing.T, path string) {
	t.Helper()
	info, err := os.Stat(path)
	if err != nil || info.Mode().Perm() != 0o600 {
		t.Fatalf("%s: info = %v, err = %v", path, info, err)
	}
}
