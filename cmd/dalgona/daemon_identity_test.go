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
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/daemon"
)

func servePipe(t *testing.T, extensions map[string]rpcserver.Extension) net.Conn {
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
		_ = rpcserver.ServeWith(t.Context(), serverConn, host, rpcserver.ServeOptions{Extensions: extensions})
		_ = closeHost()
	}()
	t.Cleanup(func() {
		_ = clientConn.Close()
		<-done
	})
	return clientConn
}

func TestDaemonIdentityAnswersBeforeInitialize(t *testing.T) {
	conn := servePipe(t, map[string]rpcserver.Extension{client.MethodIdentity: identityExtension(time.Now())})
	peer := rpc.Dial(t.Context(), conn, quietClient{})
	defer func() { _ = peer.Close() }()
	var info client.Info
	if err := peer.Call(t.Context(), client.MethodIdentity, nil, &info); err != nil {
		t.Fatalf("identity as first message: %v", err)
	}
	if info.Build != client.Build() {
		t.Fatalf("identity build = %q, want %q", info.Build, client.Build())
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
	conn := servePipe(t, map[string]rpcserver.Extension{client.MethodRetire: retireExtension(func() { close(retired) })})
	peer := rpc.Dial(t.Context(), conn, quietClient{})
	defer func() { _ = peer.Close() }()
	callErr := peer.Call(t.Context(), client.MethodRetire, nil, nil)
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
	startedAt := time.Now()
	newHost := func(ctx context.Context) (*agent.Host, func() error, error) {
		return app.NewHost(ctx, app.Options{Product: config.ProductDalgona})
	}
	serve := func(ctx context.Context, conn io.ReadWriteCloser, host *agent.Host) error {
		extensions := map[string]rpcserver.Extension{}
		if build != "" {
			extensions[client.MethodIdentity] = func(context.Context, jsontext.Value) (jsontext.Value, error) {
				reply, err := json.Marshal(client.Info{Build: build, PID: pid, Version: "test", StartedAt: startedAt})
				if err != nil {
					return nil, err
				}
				return jsontext.Value(reply), nil
			}
		}
		if retireCancel != nil {
			extensions[client.MethodRetire] = func(context.Context, jsontext.Value) (jsontext.Value, error) {
				retireCancel()
				return jsontext.Value("{}"), nil
			}
		}
		return rpcserver.ServeWith(ctx, conn, host, rpcserver.ServeOptions{Extensions: extensions})
	}
	go func() { done <- daemon.Run(runCtx, home, newHost, serve) }()
	deadline := time.Now().Add(5 * time.Second)
	for {
		if conn, err := net.DialTimeout("unix", client.SocketPath(home), 200*time.Millisecond); err == nil {
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

func testBackend(t *testing.T) client.Backend {
	t.Helper()
	backend, err := client.ResolveBackend(os.Args[0])
	if err != nil {
		t.Fatal(err)
	}
	return backend
}

func TestEnsureRefusesOtherBuild(t *testing.T) {
	home := shortDaemonHome(t)
	cancel, done := startFakeDaemon(t, home, otherBuild, os.Getpid(), nil)
	defer stopFakeDaemon(t, cancel, done)
	session := client.Local(home, testBackend(t))
	defer func() { _ = session.Close() }()
	_, err := session.Connect(t.Context())
	mismatch, ok := errors.AsType[*client.BuildMismatchError](err)
	if !ok {
		t.Fatalf("connect error = %v, want BuildMismatchError", err)
	}
	want := fmt.Sprintf("daemon runs build %s, this client is build %s; run dalgona restart", otherBuild, client.Build())
	if mismatch.Error() != want {
		t.Fatalf("connect message = %q, want %q", mismatch.Error(), want)
	}
}

func TestEnsureDeclinedRestartRefuses(t *testing.T) {
	home := shortDaemonHome(t)
	cancel, done := startFakeDaemon(t, home, otherBuild, os.Getpid(), nil)
	defer stopFakeDaemon(t, cancel, done)
	session := client.Local(home, testBackend(t))
	defer func() { _ = session.Close() }()
	out := &strings.Builder{}
	conn, err := connectPrompting(t.Context(), session, true, strings.NewReader("\n"), out)
	if conn != nil {
		_ = conn.Close()
		t.Fatal("declined restart connected")
	}
	if err == nil || err.Error() != "daemon build mismatch: restart the daemon with dalgona restart" {
		t.Fatalf("declined restart error = %v", err)
	}
	prompt := fmt.Sprintf("daemon %s runs build %s; this client is build %s. Restart the daemon? Running turns stop and recover. [y/N] ",
		home, otherBuild, client.Build())
	if !strings.HasPrefix(out.String(), prompt) {
		t.Fatalf("prompt = %q, want prefix %q", out.String(), prompt)
	}
}

func TestEnsureAcceptedRestartStartsTheSelectedBackend(t *testing.T) {
	home := shortDaemonHome(t)
	retired := make(chan struct{})
	var cancel context.CancelFunc
	var done <-chan error
	cancel, done = startFakeDaemon(t, home, otherBuild, os.Getpid(), func() {
		close(retired)
		cancel()
	})
	t.Setenv("DALGONA_TEST_BACKEND", "1")
	t.Cleanup(func() { retireSpawnedDaemon(t, home) })
	session := client.Local(home, testBackend(t))
	defer func() { _ = session.Close() }()
	conn, err := connectPrompting(t.Context(), session, true, strings.NewReader("y\n"), io.Discard)
	if err != nil {
		t.Fatalf("accepted restart: %v", err)
	}
	_ = conn.Close()
	select {
	case <-retired:
	case <-time.After(5 * time.Second):
		t.Fatal("retire did not cancel the daemon context")
	}
	stopFakeDaemon(t, cancel, done)
	info, probe, err := client.ProbeDaemon(t.Context(), home)
	if err != nil || probe != client.ProbeReady {
		t.Fatalf("probe after restart = %v, %v", probe, err)
	}
	if info.Build != client.Build() || info.PID == os.Getpid() {
		t.Fatalf("restarted daemon = %+v, want the selected backend build %s in another process", info, client.Build())
	}
}

func TestRestartRefusesAReplacedDaemon(t *testing.T) {
	home := shortDaemonHome(t)
	cancel, done := startFakeDaemon(t, home, otherBuild, os.Getpid(), func() { t.Error("replaced daemon was retired") })
	defer stopFakeDaemon(t, cancel, done)
	session := client.Local(home, testBackend(t))
	defer func() { _ = session.Close() }()
	seen := client.Info{Build: otherBuild, PID: os.Getpid(), StartedAt: time.Now().Add(-time.Hour)}
	_, err := session.Restart(t.Context(), seen)
	if _, ok := errors.AsType[*client.ReplacedError](err); !ok {
		t.Fatalf("restart error = %v, want ReplacedError", err)
	}
	if _, err := os.Lstat(client.SocketPath(home)); err != nil {
		t.Fatalf("replaced daemon socket: %v", err)
	}
}

func serveTestBackend(home string) int {
	if err := client.TightenHome(home); err != nil {
		return 1
	}
	listener, err := net.Listen("unix", client.SocketPath(home))
	if err != nil {
		return 1
	}
	if err := os.Chmod(client.SocketPath(home), 0o600); err != nil {
		return 1
	}
	ctx, retire := context.WithCancel(context.Background())
	go func() {
		<-ctx.Done()
		_ = listener.Close()
	}()
	serveBackendConns(listener, testBackendHandler(retire))
	return 0
}

func testBackendHandler(retire context.CancelFunc) rpc.Handler {
	identity, retirement := identityExtension(time.Now()), retireExtension(retire)
	return func(ctx context.Context, method string, params jsontext.Value) (jsontext.Value, error) {
		switch method {
		case client.MethodIdentity:
			return identity(ctx, params)
		case client.MethodRetire:
			return retirement(ctx, params)
		default:
			return nil, &rpc.RPCError{Code: rpc.CodeMethodNotFound, Message: "method not found"}
		}
	}
}

func serveBackendConns(listener net.Listener, handler rpc.Handler) {
	var conns sync.WaitGroup
	for {
		conn, err := listener.Accept()
		if err != nil {
			break
		}
		conns.Go(func() {
			served := rpc.NewServerConn(context.Background(), conn, handler)
			served.Start()
			<-served.Done()
		})
	}
	finished := make(chan struct{})
	go func() {
		conns.Wait()
		close(finished)
	}()
	select {
	case <-finished:
	case <-time.After(2 * time.Second):
	}
}

func retireSpawnedDaemon(t *testing.T, home string) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if _, probe, _ := client.ProbeDaemon(ctx, home); probe != client.ProbeReady {
		return
	}
	if err := client.Retire(ctx, home); err != nil {
		t.Errorf("retire spawned daemon: %v", err)
	}
	if !client.WaitSocketGone(ctx, home) {
		t.Error("spawned daemon did not stop")
	}
}

func TestRestartRefusesPreIdentityDaemon(t *testing.T) {
	home := shortDaemonHome(t)
	cancel, done := startFakeDaemon(t, home, "", 0, nil)
	defer stopFakeDaemon(t, cancel, done)
	code := restartDaemon(t.Context(), home, stdio{in: strings.NewReader(""), out: io.Discard, errOut: io.Discard})
	if code != 1 {
		t.Fatalf("restart exit = %d, want 1", code)
	}
	if _, err := os.Lstat(client.SocketPath(home)); err != nil {
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
	if _, err := os.Lstat(client.SocketPath(home)); err != nil {
		t.Fatalf("unverified daemon was stopped or removed: %v", err)
	}
}
