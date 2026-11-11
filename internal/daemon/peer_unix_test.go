//go:build unix

package daemon

import (
	"net"
	"os"
	"path/filepath"
	"testing"
)

func TestVerifyHomeRefusesLooseMode(t *testing.T) {
	home := shortHome(t)
	if err := verifyHome(home); err != nil {
		t.Fatalf("tight home refused: %v", err)
	}
	if err := os.Chmod(home, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := verifyHome(home); err == nil {
		t.Fatal("loose home accepted")
	}
}

func TestVerifySocketOwnership(t *testing.T) {
	home := shortHome(t)
	sock := SocketPath(home)
	listener, err := net.Listen("unix", sock)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	if verr := verifySocket(sock); verr != nil {
		t.Fatalf("own socket refused: %v", verr)
	}
	if verr := verifySocket(filepath.Join(home, "absent.sock")); verr == nil {
		t.Fatal("missing socket accepted")
	}
}
