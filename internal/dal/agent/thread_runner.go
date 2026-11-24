package agent

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

var ErrExecNotStarted = errors.New("agent: exec did not start")

func (t *Thread) Runner() runner.Runner { return t.runner }

func (t *Thread) Cwd() string { return t.created.Cwd }

func (t *Thread) RunnerName() string { return t.created.Runner }

func (t *Thread) BindingRevision() (string, bool, error) {
	v, err := t.view(context.Background())
	if err != nil {
		return "", false, err
	}
	bound := v.state.Runner
	return BindingRevision(t.created.Runner, bound), bound != nil && bound.BoxID != "", nil
}

func BindingRevision(runnerName string, bound *event.RunnerBound) string {
	digest := sha256.New()
	fields := []string{runnerName}
	if bound != nil {
		fields = append(fields, bound.Runner, bound.Service, bound.BoxID, bound.Baseline)
	}
	for _, field := range fields {
		digest.Write([]byte(field))
		digest.Write([]byte{0})
	}
	return hex.EncodeToString(digest.Sum(nil))
}

func (t *Thread) Exec(ctx context.Context, id string, spec ExecSpec) (ExecResult, error) {
	mode, err := t.EffectiveSandbox()
	if err != nil {
		return ExecResult{}, fmt.Errorf("%w: %w", ErrExecNotStarted, err)
	}
	roots := append([]string{t.created.Cwd}, t.host.roots...)
	process := runner.ProcessSpec{
		ID: id, Argv: spec.Argv, Dir: t.created.Cwd,
		Sandbox: deniedSocketSandbox(execSandboxMode(mode), roots, t.host.Config().Home),
	}
	if err := t.runner.Start(ctx, process); err != nil {
		return ExecResult{}, fmt.Errorf("%w: %w", ErrExecNotStarted, err)
	}
	return collectExec(ctx, t.runner, id, spec)
}
