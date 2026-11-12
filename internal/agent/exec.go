package agent

import (
	"context"
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync/atomic"
	"time"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/runner"
)

const (
	execExitTimeout = 124
	execStopGrace   = 5 * time.Second
	execPoll        = 20 * time.Millisecond
)

type ExecSpec struct {
	Argv      []string
	Dir       string
	Env       map[string]*string
	Sandbox   config.SandboxMode
	Timeout   time.Duration
	OutputCap int64
	OnOutput  func([]byte)
}

type ExecResult struct {
	ExitCode   int
	Output     []byte
	CapReached bool
	TimedOut   bool
}

func (h *Host) Exec(ctx context.Context, id string, spec ExecSpec) (ExecResult, error) {
	run, err := h.execRunner()
	if err != nil {
		return ExecResult{}, fmt.Errorf("agent: exec runner: %w", err)
	}
	process := runner.ProcessSpec{
		ID: id, Argv: spec.Argv, Dir: spec.Dir, Env: execEnvironment(spec.Env),
		Sandbox: deniedSocketSandbox(execSandboxMode(spec.Sandbox), append([]string{spec.Dir}, h.roots...), h.cfg.Config.Home),
	}
	if err := run.Start(ctx, process); err != nil {
		return ExecResult{}, fmt.Errorf("agent: exec start: %w", err)
	}
	return collectExec(ctx, run, id, spec)
}

func (h *Host) execRunner() (*runner.Local, error) {
	h.execMu.Lock()
	defer h.execMu.Unlock()
	if h.exec == nil {
		local, err := runner.NewLocal(filepath.Join(h.cfg.Config.Home, "exec"))
		if err != nil {
			return nil, err
		}
		h.exec = local
	}
	return h.exec, nil
}

func execSandboxMode(mode config.SandboxMode) runner.SandboxMode {
	if mode == config.SandboxOff {
		return runner.SandboxOff
	}
	return runner.SandboxWorkspaceWrite
}

func execEnvironment(overrides map[string]*string) []string {
	env := os.Environ()
	for _, key := range slices.Sorted(maps.Keys(overrides)) {
		env = slices.DeleteFunc(env, func(entry string) bool {
			name, _, _ := strings.Cut(entry, "=")
			return name == key
		})
		if value := overrides[key]; value != nil {
			env = append(env, key+"="+*value)
		}
	}
	return env
}

func collectExec(ctx context.Context, run runner.Runner, id string, spec ExecSpec) (ExecResult, error) {
	done := make(chan struct{})
	defer close(done)
	timedOut := stopExec(ctx, run, id, spec.Timeout, done)
	reads := context.WithoutCancel(ctx)
	var result ExecResult
	var offset int64
	for {
		out, err := run.Read(reads, id, offset)
		if err != nil {
			_ = run.Signal(reads, id, runner.SignalKill)
			return result, fmt.Errorf("agent: exec read: %w", err)
		}
		offset = out.Next
		result.keep(out.Data, spec.OutputCap, spec.OnOutput)
		if out.Exit != nil {
			result.ExitCode = out.Exit.Code
			result.TimedOut = timedOut.Load()
			if result.TimedOut {
				result.ExitCode = execExitTimeout
			}
			return result, nil
		}
		<-time.After(execPoll)
	}
}

func (r *ExecResult) keep(data []byte, limit int64, sink func([]byte)) {
	if limit > 0 {
		room := max(limit-int64(len(r.Output)), 0)
		if int64(len(data)) > room {
			data = data[:room]
			r.CapReached = true
		}
	}
	if len(data) == 0 {
		return
	}
	r.Output = append(r.Output, data...)
	if sink != nil {
		sink(data)
	}
}

func stopExec(ctx context.Context, run runner.Runner, id string, timeout time.Duration, done <-chan struct{}) *atomic.Bool {
	timedOut := new(atomic.Bool)
	go func() {
		var fire <-chan time.Time
		if timeout > 0 {
			timer := time.NewTimer(timeout)
			defer timer.Stop()
			fire = timer.C
		}
		select {
		case <-done:
			return
		case <-ctx.Done():
		case <-fire:
			timedOut.Store(true)
		}
		stop := context.WithoutCancel(ctx)
		_ = run.Signal(stop, id, runner.SignalTerm)
		grace := time.NewTimer(execStopGrace)
		defer grace.Stop()
		select {
		case <-done:
		case <-grace.C:
			_ = run.Signal(stop, id, runner.SignalKill)
		}
	}()
	return timedOut
}
