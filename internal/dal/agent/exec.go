package agent

import (
	"context"
	"errors"
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

const (
	execExitTimeout = 124
	execStopGrace   = 4 * time.Second
	execSignalLimit = 500 * time.Millisecond
	execPoll        = 20 * time.Millisecond
)

var errExecTimedOut = errors.New("agent: execution deadline reached")

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
	mode := execSandboxMode(spec.Sandbox)
	if h.cfg.SandboxCause() != "" {
		mode = runner.SandboxOff
	}
	process := runner.ProcessSpec{
		ID: id, Argv: spec.Argv, Dir: spec.Dir, Env: execEnvironment(h.cfg.ShellEnv.Resolve(ctx, os.Environ()), spec.Env), EnvResolved: true,
		Sandbox: deniedSocketSandbox(mode, append([]string{spec.Dir}, h.roots...), h.Config().Home),
	}
	if err := run.Start(ctx, process); err != nil {
		return ExecResult{}, fmt.Errorf("agent: exec start: %w", err)
	}
	return collectExec(ctx, run, id, spec)
}

func (h *Host) execRunner() (*runner.Local, error) {
	h.execStateMu.Lock()
	defer h.execStateMu.Unlock()
	if h.exec == nil {
		local, err := runner.NewLocalWithShellEnv(filepath.Join(h.Config().Home, "exec"), h.cfg.ShellEnv)
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

func execEnvironment(base []string, overrides map[string]*string) []string {
	env := slices.Clone(base)
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

type execCollector struct {
	run    runner.Runner
	id     string
	spec   ExecSpec
	offset int64
	result ExecResult
}

func collectExec(ctx context.Context, run runner.Runner, id string, spec ExecSpec) (ExecResult, error) {
	collector := execCollector{run: run, id: id, spec: spec}
	running := ctx
	if spec.Timeout > 0 {
		var cancel context.CancelFunc
		running, cancel = context.WithTimeoutCause(ctx, spec.Timeout, errExecTimedOut)
		defer cancel()
	}
	err := collector.poll(running)
	switch {
	case err == nil:
		return collector.result, nil
	case running.Err() == nil:
		collector.signal(context.WithoutCancel(ctx), runner.SignalKill)
		return collector.result, err
	}
	if err := collector.settle(ctx); err != nil {
		return collector.result, fmt.Errorf("agent: exec stop without confirmed exit: %w", err)
	}
	if errors.Is(context.Cause(running), errExecTimedOut) {
		collector.result.TimedOut = true
		collector.result.ExitCode = execExitTimeout
	}
	return collector.result, nil
}

func (c *execCollector) poll(ctx context.Context) error {
	ticker := time.NewTicker(execPoll)
	defer ticker.Stop()
	for {
		out, err := c.run.Read(ctx, c.id, c.offset)
		if err != nil {
			return fmt.Errorf("agent: exec read: %w", err)
		}
		c.offset = out.Next
		c.result.keep(out.Data, c.spec.OutputCap, c.spec.OnOutput)
		if out.Exit != nil {
			c.result.ExitCode = out.Exit.Code
			return nil
		}
		select {
		case <-ticker.C:
		case <-ctx.Done():
			return ctx.Err()
		}
	}
}

func (c *execCollector) settle(ctx context.Context) error {
	now := time.Now()
	deadline := now.Add(execStopGrace)
	if requestDeadline, ok := ctx.Deadline(); ok && requestDeadline.Before(deadline) {
		deadline = requestDeadline
	}
	stop, cancel := context.WithDeadline(context.WithoutCancel(ctx), deadline)
	defer cancel()
	term, cancelTerm := context.WithDeadline(stop, now.Add(deadline.Sub(now)*3/4))
	c.signal(term, runner.SignalTerm)
	err := c.poll(term)
	cancelTerm()
	if err == nil {
		return nil
	}
	c.signal(stop, runner.SignalKill)
	return c.poll(stop)
}

func (c *execCollector) signal(ctx context.Context, signal runner.Signal) {
	ctx, cancel := context.WithTimeout(ctx, execSignalLimit)
	defer cancel()
	_ = c.run.Signal(ctx, c.id, signal)
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
