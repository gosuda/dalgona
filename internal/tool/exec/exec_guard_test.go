package exec

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
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
}
