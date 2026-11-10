//go:build windows

package daemon

import (
	"os"
	"path/filepath"
	"testing"
)

func TestDialPathAcceptsUpgradedHome(t *testing.T) {
	home := filepath.Join(t.TempDir(), "home")
	if err := os.MkdirAll(home, 0o700); err != nil {
		t.Fatalf("MkdirAll: %v", err)
	}
	if err := tightenHomeRoot(home); err != nil {
		t.Fatalf("tightenHomeRoot: %v", err)
	}
	if err := verifyHome(home); err != nil {
		t.Fatalf("verifyHome: %v", err)
	}
}
