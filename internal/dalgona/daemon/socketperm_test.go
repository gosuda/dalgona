//go:build !windows

package daemon

import (
	"os"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/client"
)

func TestListenSetsOwnerOnlyPermissions(t *testing.T) {
	home := shortHome(t)
	if err := os.Chmod(home, 0o755); err != nil { //nolint:gosec // deliberate loose permissions to test tightening
		t.Fatal(err)
	}
	if err := client.TightenHome(home); err != nil {
		t.Fatal(err)
	}
	listener, err := listen(home)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	info, err := os.Stat(client.SocketPath(home))
	if err != nil {
		t.Fatal(err)
	}
	if got := info.Mode().Perm(); got != 0o600 {
		t.Fatalf("socket mode = %04o, want 0600", got)
	}
	homeInfo, err := os.Stat(home)
	if err != nil {
		t.Fatal(err)
	}
	if got := homeInfo.Mode().Perm(); got != 0o700 {
		t.Fatalf("home mode = %04o, want 0700", got)
	}
}
