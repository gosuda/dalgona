package exec

import (
	"context"
	"encoding/json/jsontext"
	"fmt"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

type deadlineReadRunner struct {
	runner.Runner
	mu       sync.Mutex
	signaled bool
}

func (f *deadlineReadRunner) Start(context.Context, runner.ProcessSpec) error {
	return nil
}

func (f *deadlineReadRunner) Signal(context.Context, string, runner.Signal) error {
	f.mu.Lock()
	f.signaled = true
	f.mu.Unlock()
	return nil
}

func (f *deadlineReadRunner) Read(ctx context.Context, _ string, _ int64) (runner.Output, error) {
	f.mu.Lock()
	signaled := f.signaled
	f.mu.Unlock()
	if signaled {
		return runner.Output{
			Data: []byte("early"),
			Next: int64(len("early")),
			Exit: &runner.Exit{Code: 143},
			Path: "runs/timeout-race/out",
		}, nil
	}
	<-ctx.Done()
	return runner.Output{}, fmt.Errorf("runner: read: %w", ctx.Err())
}

func TestExecTimeoutReadDeadline(t *testing.T) {
	fake := &deadlineReadRunner{}
	env := tool.Env{
		Runner:  fake,
		Cwd:     t.TempDir(),
		Sandbox: runner.Sandbox{Mode: runner.SandboxOff},
	}
	toolExec := New()
	toolExec.grace = 300 * time.Millisecond
	result, runErr := toolExec.Run(t.Context(), tool.Call{
		ID:   "timeout-race",
		Args: jsontext.Value(`{"command":"true","timeout_ms":200}`),
	}, env)
	if runErr == nil {
		t.Fatal("timeout race run returned no error")
	}
	if !strings.Contains(runErr.Error(), "timed out") {
		t.Fatalf("error = %v", runErr)
	}
	if result.OK {
		t.Fatal("timed out call reported success")
	}
	if result.ExitCode == nil {
		t.Fatal("timed out call settled without an exit record")
	}
	if !strings.Contains(string(result.Output), "early") {
		t.Fatalf("output before the timeout is missing: %q", result.Output)
	}
}
