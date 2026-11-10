package daemon

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestEnsureRejectsLongSocketPath(t *testing.T) {
	parent := t.TempDir()
	home := filepath.Join(parent, strings.Repeat("h", 100))
	if err := os.MkdirAll(home, 0o700); err != nil {
		t.Fatal(err)
	}
	sock := SocketPath(home)
	if len(sock) < socketPathLimit {
		t.Fatalf("socket %q is %d bytes, want at least %d", sock, len(sock), socketPathLimit)
	}
	_, err := Ensure(t.Context(), home, "nonexistent-dalgona")
	want := fmt.Sprintf("daemon socket path %s is %d bytes; the limit is %d; use a shorter home directory", sock, len(sock), socketPathLimit)
	if err == nil || err.Error() != want {
		t.Fatalf("err=%v, want %q", err, want)
	}
	if _, statErr := os.Stat(filepath.Join(home, "daemon.log")); !os.IsNotExist(statErr) {
		t.Fatalf("daemon.log present, spawn ran")
	}
}

func TestCheckSocketPathLimitBoundary(t *testing.T) {
	parent := t.TempDir()
	room := socketPathLimit - len(SocketPath(parent)) - 2
	if room < 1 {
		t.Skip("temp dir path too long for boundary probe")
	}
	under := filepath.Join(parent, strings.Repeat("h", room))
	if got := len(SocketPath(under)); got != socketPathLimit-1 {
		t.Fatalf("socket is %d bytes, want %d", got, socketPathLimit-1)
	}
	if err := checkSocketPathLimit(under); err != nil {
		t.Fatalf("under limit: %v", err)
	}
	if err := checkSocketPathLimit(under + "h"); err == nil {
		t.Fatal("at-limit socket path passed")
	}
}
