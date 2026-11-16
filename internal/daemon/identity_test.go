package daemon

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"io"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/rpc"
)

type identityReply struct {
	Build     string `json:"build"`
	PID       int    `json:"pid"`
	Version   string `json:"version"`
	StartedAt string `json:"startedAt"`
}

func identityServe(build string, pid int, retireCancel func()) func(context.Context, io.ReadWriteCloser, *agent.Host) error {
	return func(ctx context.Context, conn io.ReadWriteCloser, host *agent.Host) error {
		extensions := map[string]rpc.Extension{}
		if build != "" {
			extensions[MethodIdentity] = func(context.Context, jsontext.Value) (jsontext.Value, error) {
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
			extensions[MethodRetire] = func(context.Context, jsontext.Value) (jsontext.Value, error) {
				retireCancel()
				return jsontext.Value("{}"), nil
			}
		}
		return rpc.ServeWith(ctx, conn, host, rpc.ServeOptions{Extensions: extensions})
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

func ownDigest(t *testing.T, extra []byte) string {
	t.Helper()
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	if resolved, resolveErr := filepath.EvalSymlinks(exe); resolveErr == nil {
		exe = resolved
	}
	data, err := os.ReadFile(exe)
	if err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256(append(data, extra...))
	return hex.EncodeToString(digest[:])[:16]
}

func TestBuildMatchesExecutableDigest(t *testing.T) {
	want := ownDigest(t, nil)
	if got := Build(); got != want {
		t.Fatalf("Build() = %q, want %q", got, want)
	}
	if got := Build(); got != want {
		t.Fatalf("second Build() = %q, want %q", got, want)
	}
	if altered := ownDigest(t, []byte{0}); altered == Build() {
		t.Fatal("digest of an altered executable equals Build(), so the negative control cannot fail")
	}
}

func TestCheckBuildRejectsOtherBuild(t *testing.T) {
	home := shortHome(t)
	other := "00112233445566ff"
	cancel, done := startServe(t, t.Context(), home, identityServe(other, os.Getpid(), nil))
	defer stopRun(t, cancel, done)
	_, err := CheckBuild(t.Context(), home, Build())
	mismatch, ok := errors.AsType[*BuildMismatchError](err)
	if !ok {
		t.Fatalf("CheckBuild error = %v, want BuildMismatchError", err)
	}
	if mismatch.Running != other || mismatch.Own != Build() {
		t.Fatalf("mismatch = %q vs %q, want %q vs %q", mismatch.Running, mismatch.Own, other, Build())
	}
	if _, err := CheckBuild(t.Context(), home, other); err != nil {
		t.Fatalf("CheckBuild with the running build = %v, want nil", err)
	}
}

func TestCheckBuildTreatsMethodNotFoundAsUnknownBuild(t *testing.T) {
	home := shortHome(t)
	cancel, done := startServe(t, t.Context(), home, identityServe("", 0, nil))
	defer stopRun(t, cancel, done)
	_, err := CheckBuild(t.Context(), home, Build())
	mismatch, ok := errors.AsType[*BuildMismatchError](err)
	if !ok {
		t.Fatalf("CheckBuild error = %v, want BuildMismatchError", err)
	}
	if mismatch.Running != UnknownPreIdentityBuild {
		t.Fatalf("mismatch running = %q, want %q", mismatch.Running, UnknownPreIdentityBuild)
	}
}

func TestRetireCancelsDaemonContext(t *testing.T) {
	home := shortHome(t)
	retired := make(chan struct{})
	ctx, cancelDaemon := context.WithCancel(t.Context())
	defer cancelDaemon()
	cancel, done := startServe(t, ctx, home, identityServe(Build(), os.Getpid(), func() {
		close(retired)
		cancelDaemon()
	}))
	defer cancel()
	if err := Retire(t.Context(), home); err != nil {
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
