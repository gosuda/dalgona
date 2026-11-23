//go:build linux

package client

import (
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"sync/atomic"
	"testing"
	"testing/iotest"
	"time"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"golang.org/x/sys/unix"
)

const probeSockName = "dalgona.sock"

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
			if perr := VerifyAcceptedPeer(conn, false); perr != nil {
				select {
				case refused <- perr:
				default:
				}
				_ = conn.Close()
				continue
			}
			served.Add(1)
			_, _ = io.ReadFull(conn, make([]byte, 4))
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
	dir, err := os.MkdirTemp("", "dg")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return filepath.Join(dir, probeSockName)
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
	refuseNNPChild(t, sock, refused)
	t.Run("landlock", func(t *testing.T) {
		if runner.UnavailableCause() != "" {
			t.Skip("Landlock ABI is 0")
		}
		refuseLandlockHelper(t, sock, refused)
	})
	if got := served.Load(); got != wantServed {
		t.Fatalf("served = %d, want %d", got, wantServed)
	}
}

func refuseNNPChild(t *testing.T, sock string, refused <-chan error) {
	t.Helper()
	cmd := childWithEnv(t, "DALGONA_TEST_DIAL=1", "DALGONA_TEST_NNP=1")
	cmd.Dir = filepath.Dir(sock)
	code, out := childExitCode(t, cmd)
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
		return true, runNNPProbe()
	}
	return true, runFileProbe()
}

func runNNPProbe() int {
	if os.Getenv("DALGONA_TEST_NNP_SEALED") == "" {
		return sealAndReexec()
	}
	marked, err := peerSelfNoNewPrivs()
	if err != nil || !marked {
		_, _ = fmt.Fprintln(os.Stderr, "sandbox probe has no no_new_privs mark:", err)
		return 2
	}
	sock, err := probeSockFromCwd()
	if err != nil {
		return 2
	}
	return dialProbe(sock)
}

func sealAndReexec() int {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	if err := unix.Prctl(unix.PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0); err != nil {
		return 2
	}
	exe, err := os.Executable()
	if err != nil {
		return 2
	}
	if err := unix.Exec(exe, []string{exe}, append(os.Environ(), "DALGONA_TEST_NNP_SEALED=1")); err != nil {
		return 2
	}
	return 2
}

func probeSockFromCwd() (string, error) {
	dir, err := os.Getwd()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, probeSockName), nil
}

func runFileProbe() int {
	sock, err := dialTarget()
	if err != nil {
		return 2
	}
	return dialProbe(sock)
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

func serveHalfCloseHello(listener net.Listener) <-chan error {
	result := make(chan error, 1)
	go func() {
		sconn, acceptErr := listener.Accept()
		if acceptErr != nil {
			result <- acceptErr
			return
		}
		defer func() { _ = sconn.Close() }()
		_ = sconn.SetDeadline(time.Now().Add(10 * time.Second))
		if _, readErr := io.ReadFull(sconn, make([]byte, 4)); readErr != nil {
			result <- readErr
			return
		}
		if _, writeErr := sconn.Write([]byte("hello")); writeErr != nil {
			result <- writeErr
			return
		}
		if uc, ok := sconn.(*net.UnixConn); ok {
			_ = uc.CloseWrite()
		}
		_, _ = io.ReadFull(sconn, make([]byte, 1))
		result <- nil
	}()
	return result
}

func TestSandboxedClientAfterDataEOFRemainsEOF(t *testing.T) {
	dir := t.TempDir()
	sock := filepath.Join(dir, "sock")
	listener, listenErr := net.Listen("unix", sock)
	if listenErr != nil {
		t.Fatal(listenErr)
	}
	t.Cleanup(func() { _ = listener.Close() })
	served := serveHalfCloseHello(listener)
	conn, dialErr := dialPeer(sock)
	if dialErr != nil {
		t.Fatal(dialErr)
	}
	defer func() { _ = conn.Close() }()
	_ = conn.SetDeadline(time.Now().Add(10 * time.Second))
	if _, writeErr := conn.Write([]byte("ping")); writeErr != nil {
		t.Fatal(writeErr)
	}
	var got [5]byte
	if _, readErr := io.ReadFull(iotest.OneByteReader(conn), got[:]); readErr != nil {
		t.Fatalf("read response: %v", readErr)
	}
	if string(got[:]) != "hello" {
		t.Fatalf("response = %q, want %q", got, "hello")
	}
	var tail [1]byte
	_, finalErr := conn.Read(tail[:])
	if !errors.Is(finalErr, io.EOF) {
		t.Fatalf("final read = %v, want EOF", finalErr)
	}
	if errors.Is(finalErr, errSandboxedClient) {
		t.Fatalf("final EOF misclassified as sandbox: %v", finalErr)
	}
	_ = conn.Close()
	if serveErr := <-served; serveErr != nil {
		t.Fatalf("server: %v", serveErr)
	}
}
