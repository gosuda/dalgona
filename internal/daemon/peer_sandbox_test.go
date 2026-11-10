//go:build linux

package daemon

import (
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"golang.org/x/sys/unix"
)

func serveGuarded(sock string, refused chan<- error, served *atomic.Int32) (net.Listener, error) {
	listener, err := net.Listen("unix", sock)
	if err != nil {
		return nil, err
	}
	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			if perr := verifyAcceptedPeer(conn, false); perr != nil {
				select {
				case refused <- perr:
				default:
				}
				_ = conn.Close()
				continue
			}
			served.Add(1)
			_, _ = conn.Write([]byte("ok"))
			_ = conn.Close()
		}
	}()
	return listener, nil
}

func dialExpectOK(t *testing.T, sock string) {
	t.Helper()
	conn, err := net.DialTimeout("unix", sock, 10*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = conn.Close() }()
	_ = conn.SetDeadline(time.Now().Add(10 * time.Second))
	if _, err := conn.Write([]byte("ping")); err != nil {
		t.Fatal(err)
	}
	buf := make([]byte, 2)
	if _, err := io.ReadFull(conn, buf); err != nil {
		t.Fatal(err)
	}
	if string(buf) != "ok" {
		t.Fatalf("answer = %q, want ok", buf)
	}
}

func assertNoRefusal(t *testing.T, refused <-chan error) {
	t.Helper()
	select {
	case err := <-refused:
		t.Fatalf("normal client refused: %v", err)
	default:
	}
}

func awaitSandboxedRefusal(t *testing.T, refused <-chan error) {
	t.Helper()
	select {
	case perr := <-refused:
		if !errors.Is(perr, errSandboxed) {
			t.Fatalf("refusal = %v, want sandboxed peer", perr)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("no refusal recorded for sandboxed child")
	}
}

func childWithEnv(t *testing.T, kv ...string) *exec.Cmd {
	t.Helper()
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(exe)
	cmd.Env = append(os.Environ(), kv...)
	return cmd
}

func childExitCode(t *testing.T, cmd *exec.Cmd) (int, []byte) {
	t.Helper()
	out, err := cmd.CombinedOutput()
	if err == nil {
		return 0, out
	}
	if exitErr, ok := errors.AsType[*exec.ExitError](err); ok {
		return exitErr.ExitCode(), out
	}
	t.Fatalf("child failed: %v: %s", err, out)
	return -1, out
}

func probeRendezvous(t *testing.T) string {
	t.Helper()
	dir := filepath.Join(os.TempDir(), "dalgona-probe-"+strconv.Itoa(os.Getpid()))
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return filepath.Join(dir, "dalgona.sock")
}

func TestAcceptRefusesSandboxedPeer(t *testing.T) {
	hostMarked, err := peerSelfNoNewPrivs()
	if err != nil {
		t.Fatal(err)
	}
	sock := probeRendezvous(t)
	refused := make(chan error, 16)
	var served atomic.Int32
	listener, err := serveGuarded(sock, refused, &served)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	var wantServed int32
	if !hostMarked {
		dialExpectOK(t, sock)
		wantServed = 1
		if got := served.Load(); got != wantServed {
			t.Fatalf("served = %d, want %d", got, wantServed)
		}
		assertNoRefusal(t, refused)
	}
	refuseNNPChild(t, refused)
	t.Run("landlock", func(t *testing.T) {
		if !runner.Available() {
			t.Skip("Landlock ABI is 0")
		}
		refuseLandlockHelper(t, sock, refused)
	})
	if got := served.Load(); got != wantServed {
		t.Fatalf("served = %d, want %d", got, wantServed)
	}
}

func refuseNNPChild(t *testing.T, refused <-chan error) {
	t.Helper()
	code, out := childExitCode(t, childWithEnv(t, "DALGONA_TEST_DIAL=1", "DALGONA_TEST_NNP=1"))
	if code != 4 {
		t.Fatalf("sandboxed child exit = %d, want 4: %s", code, out)
	}
	if !strings.Contains(string(out), "sandbox") {
		t.Fatalf("sandboxed child error names no sandbox: %s", out)
	}
	awaitSandboxedRefusal(t, refused)
}

func refuseLandlockHelper(t *testing.T, sock string, refused <-chan error) {
	t.Helper()
	runs := filepath.Join(t.TempDir(), "runs")
	local, err := runner.NewLocal(runs)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = local.Close() })
	workspace := filepath.Join(t.TempDir(), "workspace")
	if err := os.MkdirAll(workspace, 0o700); err != nil {
		t.Fatal(err)
	}
	writeDialTarget(t, workspace, sock)
	spec := runner.ProcessSpec{
		ID:   "dial",
		Argv: []string{"@self"},
		Dir:  workspace,
		Env:  append(os.Environ(), "DALGONA_TEST_DIAL=1"),
		Sandbox: runner.Sandbox{
			Mode:          runner.SandboxWorkspaceWrite,
			WritableRoots: []string{workspace},
		},
	}
	if err := local.Start(t.Context(), spec); err != nil {
		t.Fatal(err)
	}
	state := waitDialExit(t, local)
	if state.Exit == nil {
		t.Fatal("dial process has no exit")
	}
	if state.Exit.Code != 4 {
		t.Fatalf("sandboxed dial exit = %d, want 4: %s", state.Exit.Code, state.Data)
	}
	awaitSandboxedRefusal(t, refused)
}

func writeDialTarget(t *testing.T, dir, sock string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(dir, "dial-target"), []byte(sock+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
}

func waitDialExit(t *testing.T, local *runner.Local) runner.Output {
	t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for {
		state, err := local.Read(t.Context(), "dial", 0)
		if err != nil {
			t.Fatal(err)
		}
		if state.Exit != nil {
			return state
		}
		if time.Now().After(deadline) {
			t.Fatal("sandboxed dial did not exit")
		}
		time.Sleep(50 * time.Millisecond)
	}
}

func dispatchDialProbe() (bool, int) {
	if os.Getenv("DALGONA_TEST_DIAL") == "" {
		return false, 0
	}
	if os.Getenv("DALGONA_TEST_NNP") != "" {
		if os.Getenv("DALGONA_TEST_NNP_SEALED") == "" {
			if err := unix.Prctl(unix.PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0); err != nil {
				return true, 2
			}
			exe, err := os.Executable()
			if err != nil {
				return true, 2
			}
			if err := unix.Exec(exe, []string{exe}, append(os.Environ(), "DALGONA_TEST_NNP_SEALED=1")); err != nil {
				return true, 2
			}
		}
		return true, dialProbe(probeSock())
	}
	sock, err := dialTarget()
	if err != nil {
		return true, 2
	}
	return true, dialProbe(sock)
}

func probeSock() string {
	return filepath.Join(os.TempDir(), "dalgona-probe-"+strconv.Itoa(os.Getppid()), "dalgona.sock")
}

func dialTarget() (string, error) {
	data, err := os.ReadFile("dial-target")
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(string(data)), nil
}

func dialProbe(sock string) int {
	conn, err := dialPeer(sock)
	if err != nil {
		_, _ = fmt.Fprintln(os.Stderr, "dial:", err)
		return 3
	}
	defer func() { _ = conn.Close() }()
	_ = conn.SetDeadline(time.Now().Add(10 * time.Second))
	if _, err := conn.Write([]byte("ping")); err != nil {
		_, _ = fmt.Fprintln(os.Stderr, "write:", err)
		return 4
	}
	buf := make([]byte, 2)
	if _, err := io.ReadFull(conn, buf); err != nil {
		_, _ = fmt.Fprintln(os.Stderr, "read:", err)
		return 4
	}
	if string(buf) != "ok" {
		_, _ = fmt.Fprintln(os.Stderr, "unexpected answer")
		return 5
	}
	return 0
}
