package exec

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

type failingReadRunner struct {
	runner.Runner
	err error
}

func (failingReadRunner) Start(context.Context, runner.ProcessSpec) error { return nil }

func (r failingReadRunner) Read(context.Context, string, int64) (runner.Output, error) {
	return runner.Output{}, r.err
}

func (failingReadRunner) Signal(context.Context, string, runner.Signal) error { return nil }

type cancelledReadRunner struct{ runner.Runner }

func (cancelledReadRunner) Start(context.Context, runner.ProcessSpec) error { return nil }

func (cancelledReadRunner) Read(ctx context.Context, _ string, _ int64) (runner.Output, error) {
	if err := ctx.Err(); err != nil {
		return runner.Output{}, err
	}
	return runner.Output{Exit: &runner.Exit{Code: 143}}, nil
}

func (cancelledReadRunner) Signal(context.Context, string, runner.Signal) error { return nil }

func quickExec() *Tool { return &Tool{grace: time.Millisecond, killWait: time.Millisecond} }

func TestExecMarksRunnerReadFailureInternal(t *testing.T) {
	readErr := errors.New("process log unreadable")
	env := tool.Env{Runner: failingReadRunner{err: readErr}, Cwd: t.TempDir(), Sandbox: runner.Sandbox{Mode: runner.SandboxOff}}
	result, err := quickExec().Run(t.Context(), tool.Call{ID: "read-failure", Args: jsontext.Value(`{"command":"echo hi"}`)}, env)
	if !errors.Is(err, readErr) {
		t.Fatalf("error = %v, want the read error", err)
	}
	if result.Failure != tool.FailureInternal {
		t.Fatalf("failure = %q, want a runner read failure marked internal", result.Failure)
	}
}

func TestExecLeavesCancelledWaitOrdinary(t *testing.T) {
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	env := tool.Env{Runner: cancelledReadRunner{}, Cwd: t.TempDir(), Sandbox: runner.Sandbox{Mode: runner.SandboxOff}}
	result, err := quickExec().Run(ctx, tool.Call{ID: "cancelled-wait", Args: jsontext.Value(`{"command":"echo hi"}`)}, env)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("error = %v, want cancellation", err)
	}
	if result.Failure != tool.FailureNone {
		t.Fatalf("failure = %q, want a cancelled wait to stay ordinary", result.Failure)
	}
}
