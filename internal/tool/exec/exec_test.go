package exec

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

type fakeMasker struct{}

func (fakeMasker) Mask(text string) string { return text }

func (fakeMasker) Unmask(text string) string {
	return strings.ReplaceAll(text, "{{secret:CMD}}", "echo unmasked-secret-value")
}

func testEnv(t *testing.T) (tool.Env, func()) {
	t.Helper()
	runs, err := runner.NewLocal(filepath.Join(t.TempDir(), "runs"))
	if err != nil {
		t.Fatal(err)
	}
	cwd := t.TempDir()
	env := tool.Env{Runner: runs, Cwd: cwd, Sandbox: runner.Sandbox{Mode: runner.SandboxWorkspaceWrite, WritableRoots: []string{cwd}}}
	return env, func() { _ = runs.Close() }
}

func TestExecRunsCommand(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	toolExec := New()
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-1", Args: jsontext.Value(`{"command":"printf hello"}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	if !result.OK || string(result.Output) != "hello" || result.ExitCode == nil || *result.ExitCode != 0 {
		t.Fatalf("result ok=%v output=%q exit=%v", result.OK, result.Output, result.ExitCode)
	}
}

func TestExecUnmasksCommand(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	env.Masker = fakeMasker{}
	toolExec := New()
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-2", Args: jsontext.Value(`{"command":"{{secret:CMD}}"}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	if !result.OK || !strings.Contains(string(result.Output), "unmasked-secret-value") {
		t.Fatalf("unmasked run ok=%v output=%q", result.OK, result.Output)
	}
}

func TestExecTimeoutTerminates(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	toolExec := New()
	toolExec.grace = 300 * time.Millisecond
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-3", Args: jsontext.Value(`{"command":"sleep 30; echo late","timeout_ms":120}`)}, env)
	if err == nil {
		t.Fatal("timeout run returned no error")
	}
	if !strings.Contains(err.Error(), "timed out") {
		t.Fatalf("error = %v", err)
	}
	if result.OK {
		t.Fatal("timed out call reported success")
	}
	if strings.Contains(string(result.Output), "late") {
		t.Fatalf("output survived the timeout: %q", result.Output)
	}
}

func TestExecInterruptBeforeShimPIDSettlesWithExit(t *testing.T) {
	t.Setenv("DALGONA_SHIM_TEST_PAUSE", "pid:300ms")
	env, closeRunner := testEnv(t)
	defer closeRunner()
	toolExec := New()
	ctx, cancel := context.WithTimeout(t.Context(), 50*time.Millisecond)
	defer cancel()
	result, err := toolExec.Run(ctx, tool.Call{ID: "run-early", Args: jsontext.Value(`{"command":"touch started; sleep 30","escalate":true}`)}, env)
	if err == nil || result.OK {
		t.Fatalf("interrupted run ok=%v err=%v", result.OK, err)
	}
	if result.ExitCode == nil || *result.ExitCode == 0 {
		t.Fatalf("interrupted run settled without an exit record: exit=%v err=%v", result.ExitCode, err)
	}
	if _, statErr := os.Stat(filepath.Join(env.Cwd, "started")); !errors.Is(statErr, fs.ErrNotExist) {
		t.Fatalf("command ran after the interrupt: %v", statErr)
	}
	if _, statErr := os.Stat(filepath.Join(filepath.Dir(result.Path), "pgid")); !errors.Is(statErr, fs.ErrNotExist) {
		t.Fatalf("command group was created after the interrupt: %v", statErr)
	}
}

func TestExecMissingCommandFails(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	toolExec := New()
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-4", Args: jsontext.Value(`{"command":"dalgona-definitely-missing-binary"}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	if result.OK || result.ExitCode == nil || *result.ExitCode == 0 {
		t.Fatalf("missing binary result ok=%v exit=%v", result.OK, result.ExitCode)
	}
}

func TestExecUsesWorkspaceDirectory(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	toolExec := New()
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-5", Args: jsontext.Value(`{"command":"pwd"}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	if strings.TrimSpace(string(result.Output)) != env.Cwd {
		t.Fatalf("pwd = %q, want %q", result.Output, env.Cwd)
	}
}

func TestExecRejectsBadArguments(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	toolExec := New()
	if _, err := toolExec.Run(t.Context(), tool.Call{ID: "run-6", Args: jsontext.Value(`{"cwd":"."}`)}, env); err == nil {
		t.Fatal("missing command accepted")
	}
	if _, err := toolExec.Run(t.Context(), tool.Call{ID: "run-7", Args: jsontext.Value(`{"command":"true","timeout_ms":0}`)}, env); err == nil {
		t.Fatal("zero timeout accepted")
	}
}

func TestExecKeepsTailOfLongOutput(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	toolExec := New()
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-8", Args: jsontext.Value(`{"command":"seq 1 100000"}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Output) > 64*1024+300 {
		t.Fatalf("output length = %d", len(result.Output))
	}
	if !strings.Contains(string(result.Output), "\n100000") {
		t.Fatal("tail lost the last line")
	}
	if !strings.Contains(string(result.Output), "output truncated") {
		t.Fatal("truncation notice missing")
	}
}

func TestExecEscalateRunsUnsandboxed(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	toolExec := New()
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-9", Args: jsontext.Value(`{"command":"true","escalate":true,"justification":"retry after denial"}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	if !result.OK {
		t.Fatalf("escalated run failed: %+v", result)
	}
}

func TestExecRequiresRunner(t *testing.T) {
	toolExec := New()
	if _, err := toolExec.Run(context.Background(), tool.Call{ID: "run-10", Args: jsontext.Value(`{"command":"true"}`)}, tool.Env{}); err == nil {
		t.Fatal("missing runner accepted")
	}
}
