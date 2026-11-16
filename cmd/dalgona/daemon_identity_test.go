package main

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/daemon"
	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func servePipe(t *testing.T, extensions map[string]rpc.Extension) net.Conn {
	t.Helper()
	t.Setenv("DALGONA_HOME", t.TempDir())
	serverConn, clientConn := net.Pipe()
	host, closeHost, err := app.NewHost(t.Context(), app.Options{Product: config.ProductDalgona})
	if err != nil {
		_ = clientConn.Close()
		_ = serverConn.Close()
		t.Fatalf("app host: %v", err)
	}
	done := make(chan struct{})
	go func() {
		defer close(done)
		_ = rpc.ServeWith(t.Context(), serverConn, host, rpc.ServeOptions{Extensions: extensions})
		_ = closeHost()
	}()
	t.Cleanup(func() {
		_ = clientConn.Close()
		<-done
	})
	return clientConn
}

func TestDaemonIdentityAnswersBeforeInitialize(t *testing.T) {
	conn := servePipe(t, map[string]rpc.Extension{daemon.MethodIdentity: identityExtension(time.Now())})
	client := rpc.Dial(conn, quietClient{})
	defer func() { _ = client.Close() }()
	var info daemon.Info
	if err := client.Call(t.Context(), daemon.MethodIdentity, nil, &info); err != nil {
		t.Fatalf("identity as first message: %v", err)
	}
	if info.Build != daemon.Build() {
		t.Fatalf("identity build = %q, want %q", info.Build, daemon.Build())
	}
	if info.PID != os.Getpid() {
		t.Fatalf("identity pid = %d, want %d", info.PID, os.Getpid())
	}
	if info.Version != config.Version() {
		t.Fatalf("identity version = %q, want %q", info.Version, config.Version())
	}
	if info.StartedAt.IsZero() {
		t.Fatal("identity startedAt is zero")
	}
}

func TestDaemonRetireCancelsRun(t *testing.T) {
	retired := make(chan struct{})
	conn := servePipe(t, map[string]rpc.Extension{daemon.MethodRetire: retireExtension(func() { close(retired) })})
	client := rpc.Dial(conn, quietClient{})
	defer func() { _ = client.Close() }()
	callErr := client.Call(t.Context(), daemon.MethodRetire, nil, nil)
	if callErr != nil && !errors.Is(callErr, io.ErrClosedPipe) {
		t.Fatalf("retire call: %v", callErr)
	}
	select {
	case <-retired:
	case <-time.After(5 * time.Second):
		t.Fatal("retire did not cancel the daemon context")
	}
}

func startFakeDaemon(t *testing.T, home, build string, pid int, retireCancel func()) (context.CancelFunc, <-chan error) {
	t.Helper()
	t.Setenv("DALGONA_HOME", home)
	runCtx, stop := context.WithCancel(t.Context())
	done := make(chan error, 1)
	newHost := func(ctx context.Context) (*agent.Host, func() error, error) {
		return app.NewHost(ctx, app.Options{Product: config.ProductDalgona})
	}
	serve := func(ctx context.Context, conn io.ReadWriteCloser, host *agent.Host) error {
		extensions := map[string]rpc.Extension{}
		if build != "" {
			extensions[daemon.MethodIdentity] = func(context.Context, jsontext.Value) (jsontext.Value, error) {
				reply, err := json.Marshal(daemon.Info{Build: build, PID: pid, Version: "test", StartedAt: time.Now()})
				if err != nil {
					return nil, err
				}
				return jsontext.Value(reply), nil
			}
		}
		if retireCancel != nil {
			extensions[daemon.MethodRetire] = func(context.Context, jsontext.Value) (jsontext.Value, error) {
				retireCancel()
				return jsontext.Value("{}"), nil
			}
		}
		return rpc.ServeWith(ctx, conn, host, rpc.ServeOptions{Extensions: extensions})
	}
	go func() { done <- daemon.Run(runCtx, home, newHost, serve) }()
	deadline := time.Now().Add(5 * time.Second)
	for {
		if conn, err := net.DialTimeout("unix", daemon.SocketPath(home), 200*time.Millisecond); err == nil {
			_ = conn.Close()
			return stop, done
		}
		if time.Now().After(deadline) {
			t.Fatal("fake daemon did not listen")
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func stopFakeDaemon(t *testing.T, cancel context.CancelFunc, done <-chan error) {
	t.Helper()
	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("fake daemon: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("fake daemon did not stop")
	}
}

const otherBuild = "ffeeddccbbaa0099"

func TestEnsureRefusesOtherBuild(t *testing.T) {
	home := shortDaemonHome(t)
	cancel, done := startFakeDaemon(t, home, otherBuild, os.Getpid(), nil)
	defer stopFakeDaemon(t, cancel, done)
	err := checkBuild(t.Context(), home, daemon.Build())
	mismatch, ok := errors.AsType[*daemon.BuildMismatchError](err)
	if !ok {
		t.Fatalf("checkBuild error = %v, want BuildMismatchError", err)
	}
	want := fmt.Sprintf("daemon runs build %s, this client is build %s; run dalgona restart", otherBuild, daemon.Build())
	if mismatch.Error() != want {
		t.Fatalf("checkBuild message = %q, want %q", mismatch.Error(), want)
	}
}

func TestEnsureDeclinedRestartRefuses(t *testing.T) {
	home := shortDaemonHome(t)
	cancel, done := startFakeDaemon(t, home, otherBuild, os.Getpid(), nil)
	defer stopFakeDaemon(t, cancel, done)
	out := &strings.Builder{}
	restarted, err := confirmBuildRestart(t.Context(), home, daemon.Build(), strings.NewReader("\n"), out)
	if restarted {
		t.Fatal("declined restart reported a restart")
	}
	if err == nil || err.Error() != "daemon build mismatch: restart the daemon with dalgona restart" {
		t.Fatalf("declined restart error = %v", err)
	}
	prompt := fmt.Sprintf("daemon %s runs build %s; this client is build %s. Restart the daemon? Running turns stop and recover. [y/N] ",
		home, otherBuild, daemon.Build())
	if !strings.HasPrefix(out.String(), prompt) {
		t.Fatalf("prompt = %q, want prefix %q", out.String(), prompt)
	}
}

func TestEnsureAcceptedRestartRetiresDaemon(t *testing.T) {
	home := shortDaemonHome(t)
	retired := make(chan struct{})
	var cancel context.CancelFunc
	var done <-chan error
	cancel, done = startFakeDaemon(t, home, otherBuild, os.Getpid(), func() {
		close(retired)
		cancel()
	})
	restarted, err := confirmBuildRestart(t.Context(), home, daemon.Build(), strings.NewReader("y\n"), io.Discard)
	if err != nil {
		t.Fatalf("accepted restart: %v", err)
	}
	if !restarted {
		t.Fatal("accepted restart reported no restart")
	}
	select {
	case <-retired:
	case <-time.After(5 * time.Second):
		t.Fatal("retire did not cancel the daemon context")
	}
	stopFakeDaemon(t, cancel, done)
}

func TestRestartRefusesPreIdentityDaemon(t *testing.T) {
	home := shortDaemonHome(t)
	cancel, done := startFakeDaemon(t, home, "", 0, nil)
	defer stopFakeDaemon(t, cancel, done)
	code := restartDaemon(t.Context(), home, stdio{in: strings.NewReader(""), out: io.Discard, errOut: io.Discard})
	if code != 1 {
		t.Fatalf("restart exit = %d, want 1", code)
	}
	if _, err := os.Lstat(daemon.SocketPath(home)); err != nil {
		t.Fatalf("pre-identity daemon was stopped or removed: %v", err)
	}
}

func deadPID(t *testing.T) int {
	t.Helper()
	proc, err := os.StartProcess("/bin/true", []string{"/bin/true"}, &os.ProcAttr{})
	if err != nil {
		t.Skipf("cannot spawn a short-lived process: %v", err)
	}
	if _, waitErr := proc.Wait(); waitErr != nil {
		t.Fatalf("wait for short-lived process: %v", waitErr)
	}
	return proc.Pid
}

func TestRestartRefusesUnverifiedPID(t *testing.T) {
	home := shortDaemonHome(t)
	cancel, done := startFakeDaemon(t, home, otherBuild, deadPID(t), nil)
	defer stopFakeDaemon(t, cancel, done)
	code := restartDaemon(t.Context(), home, stdio{in: strings.NewReader(""), out: io.Discard, errOut: io.Discard})
	if code != 1 {
		t.Fatalf("restart exit = %d, want 1", code)
	}
	if _, err := os.Lstat(daemon.SocketPath(home)); err != nil {
		t.Fatalf("unverified daemon was stopped or removed: %v", err)
	}
}
