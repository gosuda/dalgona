//go:build darwin

package runner

import (
	"io"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func serveDarwinListener(t *testing.T, sock string) {
	t.Helper()
	listener, err := net.Listen("unix", sock)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			_, _ = io.ReadFull(conn, make([]byte, 4))
			_, _ = conn.Write([]byte("ok"))
			_ = conn.Close()
		}
	}()
}

type darwinDial struct {
	id        string
	workspace string
	sock      string
	denied    string
}

func runDarwinDial(t *testing.T, local *Local, dial darwinDial) Output {
	t.Helper()
	if err := os.WriteFile(filepath.Join(dial.workspace, "dial-target"), []byte(dial.sock+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	spec := ProcessSpec{
		ID:   dial.id,
		Argv: []string{"@self"},
		Dir:  dial.workspace,
		Env:  append(os.Environ(), "DALGONA_TEST_DIAL=1"),
		Sandbox: Sandbox{
			Mode:          SandboxWorkspaceWrite,
			WritableRoots: []string{dial.workspace},
			DeniedSockets: []string{dial.denied},
		},
	}
	startTestProcess(t, local, spec)
	return waitForExit(t, local, dial.id)
}

func swapSocketCase(t *testing.T, sock string) (string, bool) {
	t.Helper()
	swapped := strings.ToUpper(sock)
	if swapped == sock {
		swapped = strings.ToLower(sock)
	}
	if swapped == sock {
		return "", false
	}
	if _, err := os.Stat(swapped); err != nil {
		return "", false
	}
	return swapped, true
}

func TestLocal_darwinDeniesDaemonSocket(t *testing.T) {
	if UnavailableCause() != "" {
		t.Skip("sandbox-exec is missing")
	}
	dir := t.TempDir()
	workspace := filepath.Join(dir, "workspace")
	if err := os.MkdirAll(workspace, 0o700); err != nil {
		t.Fatal(err)
	}
	sockDir, err := os.MkdirTemp("", "dg")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(sockDir) })
	sock := filepath.Join(sockDir, "dalgona.sock")
	serveDarwinListener(t, sock)
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	t.Run("allowed", func(t *testing.T) {
		allowed := runDarwinDial(t, local, darwinDial{id: "allowed", workspace: workspace, sock: sock, denied: filepath.Join(dir, "other.sock")})
		if allowed.Exit.Code != 0 {
			t.Fatalf("sandbox-exec or helper broken: allowed dial failed with exit %d: %s", allowed.Exit.Code, allowed.Data)
		}
	})
	t.Run("denied", func(t *testing.T) {
		denied := runDarwinDial(t, local, darwinDial{id: "denied", workspace: workspace, sock: sock, denied: sock})
		if denied.Exit.Code != 6 {
			t.Fatalf("dial to denied socket exit = %d, want 6: %s", denied.Exit.Code, denied.Data)
		}
	})
	t.Run("case", func(t *testing.T) {
		swapped, ok := swapSocketCase(t, sock)
		if !ok {
			t.Skip("filesystem is case-sensitive or path has no case to swap")
		}
		masked := runDarwinDial(t, local, darwinDial{id: "masked", workspace: workspace, sock: swapped, denied: sock})
		if masked.Exit.Code != 6 {
			t.Fatalf("dial to case-swapped socket exit = %d, want 6: %s", masked.Exit.Code, masked.Data)
		}
	})
}
