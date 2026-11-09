package daemon

import (
	"context"
	"errors"
	"io"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/app"
	"github.com/gosuda/dalgona/internal/config"
)

func shortHome(t *testing.T) string {
	t.Helper()
	home, err := os.MkdirTemp("", "dg")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := os.RemoveAll(home); err != nil {
			t.Error(err)
		}
	})
	return home
}

func appHost(ctx context.Context) (*agent.Host, func() error, error) {
	return app.NewHost(ctx, app.Options{Product: config.ProductDalgona})
}

func greet(_ context.Context, conn io.ReadWriteCloser, host *agent.Host) error {
	if host == nil {
		return errors.New("serve got no host")
	}
	_, err := conn.Write([]byte("ok"))
	return err
}

func startRun(t *testing.T, home string) (context.CancelFunc, <-chan error) {
	t.Helper()
	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() { done <- Run(ctx, home, appHost, greet) }()
	waitForGreeting(t, home, done)
	return cancel, done
}

func waitForGreeting(t *testing.T, home string, done <-chan error) {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		select {
		case err := <-done:
			t.Fatalf("Run returned early: %v", err)
		default:
		}
		if greeting(home) == "ok" {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatal("daemon never accepted a connection")
}

func greeting(home string) string {
	conn, err := net.DialTimeout("unix", SocketPath(home), 200*time.Millisecond)
	if err != nil {
		return ""
	}
	defer func() { _ = conn.Close() }()
	if deadlineErr := conn.SetDeadline(time.Now().Add(2 * time.Second)); deadlineErr != nil {
		return ""
	}
	data, err := io.ReadAll(conn)
	if err != nil {
		return ""
	}
	return string(data)
}

func stopRun(t *testing.T, cancel context.CancelFunc, done <-chan error) {
	t.Helper()
	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Run: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Run did not stop after cancel")
	}
}

func TestRunSecondInstanceFailsOnLock(t *testing.T) {
	home := shortHome(t)
	t.Setenv("DALGONA_HOME", home)
	cancel, done := startRun(t, home)
	err := Run(t.Context(), home, appHost, greet)
	if err == nil || !strings.Contains(err.Error(), "another dalgona process owns "+home) {
		t.Fatalf("second Run error = %v", err)
	}
	if got := greeting(home); got != "ok" {
		t.Fatalf("first daemon lost its socket after the second Run: %q", got)
	}
	stopRun(t, cancel, done)
	if _, statErr := os.Lstat(SocketPath(home)); !errors.Is(statErr, os.ErrNotExist) {
		t.Fatalf("socket remains after Run stopped: %v", statErr)
	}
}

func TestRunReplacesStaleSocket(t *testing.T) {
	home := shortHome(t)
	t.Setenv("DALGONA_HOME", home)
	stale, err := net.Listen("unix", SocketPath(home))
	if err != nil {
		t.Fatal(err)
	}
	if unixListener, ok := stale.(*net.UnixListener); ok {
		unixListener.SetUnlinkOnClose(false)
	}
	if err := stale.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Lstat(SocketPath(home)); err != nil {
		t.Fatalf("stale socket missing before Run: %v", err)
	}
	cancel, done := startRun(t, home)
	stopRun(t, cancel, done)
}

func TestRunRejectsHostFailureBeforeTouchingSocket(t *testing.T) {
	home := shortHome(t)
	marker := filepath.Join(home, "dalgona.sock")
	if err := os.WriteFile(marker, []byte("held by another daemon"), 0o600); err != nil {
		t.Fatal(err)
	}
	failing := func(context.Context) (*agent.Host, func() error, error) { return nil, nil, errors.New("locked") }
	if err := Run(t.Context(), home, failing, greet); err == nil || err.Error() != "locked" {
		t.Fatalf("Run error = %v", err)
	}
	if data, err := os.ReadFile(marker); err != nil || string(data) != "held by another daemon" {
		t.Fatalf("Run touched the socket path before holding the lock: %q, %v", data, err)
	}
}
