package daemon

import (
	"context"
	"errors"
	"io"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

func startServe(t *testing.T, home string, serve func(context.Context, io.ReadWriteCloser, *agent.Host) error) (context.CancelFunc, <-chan error) {
	t.Helper()
	t.Setenv("DALGONA_HOME", home)
	listening := observeListen(t)
	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() { done <- Run(ctx, home, appHost, serve) }()
	select {
	case err := <-done:
		t.Fatalf("Run returned before it listened: %v", err)
	case <-listening:
	}
	return cancel, done
}

func TestCheckBuildTreatsMethodNotFoundAsUnknownBuild(t *testing.T) {
	home := shortHome(t)
	cancel, done := startServe(t, home, func(ctx context.Context, conn io.ReadWriteCloser, host *agent.Host) error {
		return rpcserver.ServeWith(ctx, conn, host, rpcserver.ServeOptions{})
	})
	defer stopRun(t, cancel, done)
	_, err := client.CheckBuild(t.Context(), home, client.Build())
	mismatch, ok := errors.AsType[*client.BuildMismatchError](err)
	if !ok {
		t.Fatalf("client.CheckBuild error = %v, want client.BuildMismatchError", err)
	}
	if mismatch.Running != client.UnknownPreIdentityBuild {
		t.Fatalf("mismatch running = %q, want %q", mismatch.Running, client.UnknownPreIdentityBuild)
	}
}
