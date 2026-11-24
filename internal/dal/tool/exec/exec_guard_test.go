package exec

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

type failingStartRunner struct {
	runner.Runner
	err error
}

func (f failingStartRunner) Start(context.Context, runner.ProcessSpec) error { return f.err }

func TestExecToolMapsKillGuard(t *testing.T) {
	guardErr := fmt.Errorf("%w: cannot signal protected process", runner.ErrProcessKillGuard)
	env := tool.Env{Runner: failingStartRunner{err: guardErr}, Cwd: t.TempDir(), Sandbox: runner.Sandbox{Mode: runner.SandboxOff}}
	result, err := New().Run(t.Context(), tool.Call{ID: "kill-guard", Args: jsontext.Value(`{"command":"kill 1"}`)}, env)
	if err != nil {
		t.Fatalf("guard refusal returned an error: %v", err)
	}
	if want := "refused: process-kill guard prevents signaling daemon or parent"; result.OK || result.ExitCode != nil || string(result.Output) != want {
		t.Fatalf("result ok=%v exit=%v output=%q, want a refusal with output %q", result.OK, result.ExitCode, result.Output, want)
	}
	if result.Failure != tool.FailureNone {
		t.Fatalf("failure = %q, want a guard refusal to stay ordinary", result.Failure)
	}
}

func TestExecToolKeepsOtherStartErrors(t *testing.T) {
	startErr := errors.New("start failed for test")
	env := tool.Env{Runner: failingStartRunner{err: startErr}, Cwd: t.TempDir(), Sandbox: runner.Sandbox{Mode: runner.SandboxOff}}
	result, err := New().Run(t.Context(), tool.Call{ID: "start-error", Args: jsontext.Value(`{"command":"echo hi"}`)}, env)
	if !errors.Is(err, startErr) {
		t.Fatalf("error = %v, want the start error", err)
	}
	if result.OK || len(result.Output) != 0 {
		t.Fatalf("result ok=%v output=%q, want the zero result", result.OK, result.Output)
	}
	if result.Failure != tool.FailureInternal {
		t.Fatalf("failure = %q, want a start failure marked internal", result.Failure)
	}
}

func TestExecToolLeavesCancelledStartOrdinary(t *testing.T) {
	for _, cause := range []error{context.Canceled, context.DeadlineExceeded} {
		startErr := fmt.Errorf("runner: start: %w", cause)
		env := tool.Env{Runner: failingStartRunner{err: startErr}, Cwd: t.TempDir(), Sandbox: runner.Sandbox{Mode: runner.SandboxOff}}
		result, err := New().Run(t.Context(), tool.Call{ID: "start-cancelled", Args: jsontext.Value(`{"command":"echo hi"}`)}, env)
		if !errors.Is(err, cause) {
			t.Fatalf("error = %v, want %v", err, cause)
		}
		if result.Failure != tool.FailureNone {
			t.Fatalf("failure = %q for %v, want a cancelled start to stay ordinary", result.Failure, cause)
		}
	}
}

func TestExecToolLeavesStartDuringCancellationOrdinary(t *testing.T) {
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	startErr := errors.New("remote: connection closed")
	env := tool.Env{Runner: failingStartRunner{err: startErr}, Cwd: t.TempDir(), Sandbox: runner.Sandbox{Mode: runner.SandboxOff}}
	result, err := New().Run(ctx, tool.Call{ID: "start-during-cancel", Args: jsontext.Value(`{"command":"echo hi"}`)}, env)
	if !errors.Is(err, startErr) {
		t.Fatalf("error = %v, want the start error", err)
	}
	if result.Failure != tool.FailureNone {
		t.Fatalf("failure = %q, want a start failure of a cancelled call to stay ordinary", result.Failure)
	}
}

func TestExecToolLeavesSandboxDowngradeRefusalOrdinary(t *testing.T) {
	env := tool.Env{Runner: failingStartRunner{err: runner.ErrSandboxDowngrade}, Cwd: t.TempDir(), Sandbox: runner.Sandbox{Mode: runner.SandboxOff}}
	result, err := New().Run(t.Context(), tool.Call{ID: "start-refused", Args: jsontext.Value(`{"command":"echo hi","escalate":true}`)}, env)
	if !errors.Is(err, runner.ErrSandboxDowngrade) {
		t.Fatalf("error = %v, want the sandbox refusal", err)
	}
	if result.Failure != tool.FailureNone {
		t.Fatalf("failure = %q, want a sandbox refusal to stay ordinary", result.Failure)
	}
}
