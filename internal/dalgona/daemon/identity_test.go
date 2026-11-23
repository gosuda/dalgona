package daemon

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"io"
	"os"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

type identityReply struct {
	Build     string `json:"build"`
	PID       int    `json:"pid"`
	Version   string `json:"version"`
	StartedAt string `json:"startedAt"`
}

func identityServe(build string, pid int, retireCancel func()) func(context.Context, io.ReadWriteCloser, *agent.Host) error {
	return func(ctx context.Context, conn io.ReadWriteCloser, host *agent.Host) error {
		extensions := map[string]rpcserver.Extension{}
		if build != "" {
			extensions[client.MethodIdentity] = func(context.Context, jsontext.Value) (jsontext.Value, error) {
				reply, err := json.Marshal(identityReply{
					Build: build, PID: pid, Version: "test", StartedAt: time.Now().Format(time.RFC3339Nano),
				})
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
}

func startServe(t *testing.T, parent context.Context, home string, serve func(context.Context, io.ReadWriteCloser, *agent.Host) error) (context.CancelFunc, <-chan error) {
	t.Helper()
	t.Setenv("DALGONA_HOME", home)
	listening := observeListen(t)
	ctx, cancel := context.WithCancel(parent)
	done := make(chan error, 1)
	go func() { done <- Run(ctx, home, appHost, serve) }()
	select {
	case err := <-done:
		t.Fatalf("Run returned before it listened: %v", err)
	case <-listening:
	}
	return cancel, done
}

func TestCheckBuildRejectsOtherBuild(t *testing.T) {
	home := shortHome(t)
	other := "00112233445566ff"
	cancel, done := startServe(t, t.Context(), home, identityServe(other, os.Getpid(), nil))
	defer stopRun(t, cancel, done)
	_, err := client.CheckBuild(t.Context(), home, client.Build())
	mismatch, ok := errors.AsType[*client.BuildMismatchError](err)
	if !ok {
		t.Fatalf("client.CheckBuild error = %v, want client.BuildMismatchError", err)
	}
	if mismatch.Running != other || mismatch.Own != client.Build() {
		t.Fatalf("mismatch = %q vs %q, want %q vs %q", mismatch.Running, mismatch.Own, other, client.Build())
	}
	if _, err := client.CheckBuild(t.Context(), home, other); err != nil {
		t.Fatalf("client.CheckBuild with the running build = %v, want nil", err)
	}
}

func TestCheckBuildTreatsMethodNotFoundAsUnknownBuild(t *testing.T) {
	home := shortHome(t)
	cancel, done := startServe(t, t.Context(), home, identityServe("", 0, nil))
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

func TestRetireCancelsDaemonContext(t *testing.T) {
	home := shortHome(t)
	retired := make(chan struct{})
	ctx, cancelDaemon := context.WithCancel(t.Context())
	defer cancelDaemon()
	cancel, done := startServe(t, ctx, home, identityServe(client.Build(), os.Getpid(), func() {
		close(retired)
		cancelDaemon()
	}))
	defer cancel()
	if err := client.Retire(t.Context(), home); err != nil {
		t.Fatalf("Retire: %v", err)
	}
	select {
	case <-retired:
	case <-time.After(5 * time.Second):
		t.Fatal("retire did not cancel the daemon context")
	}
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Run after retire: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Run did not stop after retire")
	}
}
