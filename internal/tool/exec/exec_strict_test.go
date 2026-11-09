package exec

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

var errStrictPlaceholder = errors.New("secret placeholder cannot be restored")

type strictFailMasker struct{}

func (strictFailMasker) Mask(text string) string { return text }

func (strictFailMasker) Unmask(text string) string { return text }

func (strictFailMasker) UnmaskStrict(text string) (string, error) {
	if strings.Contains(text, "{{secret:pattern0:0}}") {
		return "", errStrictPlaceholder
	}
	return text, nil
}

type startRecorder struct {
	runner.Runner
	started bool
}

func (r *startRecorder) Start(_ context.Context, _ runner.ProcessSpec) error {
	r.started = true
	return nil
}

func TestExecStrictFailsWithoutRunning(t *testing.T) {
	rec := &startRecorder{}
	env := tool.Env{
		Runner:  rec,
		Cwd:     t.TempDir(),
		Sandbox: runner.Sandbox{Mode: runner.SandboxOff},
		Masker:  strictFailMasker{},
	}
	_, err := New().Run(t.Context(), tool.Call{
		ID:   "strict-1",
		Args: jsontext.Value(`{"command":"echo {{secret:pattern0:0}}"}`),
	}, env)
	if err == nil || !strings.Contains(err.Error(), "cannot be restored") {
		t.Fatalf("Run error = %v, want placeholder restore error", err)
	}
	if rec.started {
		t.Fatal("runner started despite strict unmask failure")
	}
}
