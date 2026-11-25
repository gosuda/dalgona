package runner

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"io"
	"os/exec"
	"slices"
)

type TerminalSpec struct {
	Dir     string
	Env     []string
	Sandbox Sandbox
	Columns int
	Rows    int
}

type Terminal interface {
	io.ReadWriter
	Resize(columns, rows int) error
	Signal(signal Signal) error
	Done() <-chan struct{}
	Exit() Exit
	Close() error
}

type TerminalOpener interface {
	OpenTerminal(ctx context.Context, spec TerminalSpec) (Terminal, error)
}

var (
	errTerminalSize = errors.New("runner: terminal columns and rows must be positive")
	ErrTerminalLost = errors.New("runner: terminal execution target lost")
)

func (l *Local) OpenTerminal(ctx context.Context, spec TerminalSpec) (Terminal, error) {
	if err := ctx.Err(); err != nil {
		return nil, fmt.Errorf("runner: open terminal: %w", err)
	}
	if spec.Columns < 1 || spec.Rows < 1 {
		return nil, errTerminalSize
	}
	cmd, err := terminalCommand(spec, l.startEnvironment(ctx, ProcessSpec{Env: spec.Env}))
	if err != nil {
		return nil, err
	}
	defer releaseCommandToken(cmd)
	return startTerminal(cmd, spec.Columns, spec.Rows)
}

func terminalCommand(spec TerminalSpec, env []string) (*exec.Cmd, error) {
	sandbox := Sandbox{
		Mode:          cmp.Or(spec.Sandbox.Mode, SandboxWorkspaceWrite),
		WritableRoots: slices.Clone(spec.Sandbox.WritableRoots),
		DeniedSockets: slices.Clone(spec.Sandbox.DeniedSockets),
	}
	env = overlayEnvironment(runnerEnvironment(env, spec.Dir, sandbox), []string{"TERM=xterm-256color"})
	shell := interactiveShell(environmentValue(env, shellVariable))
	cmd := exec.Command(shell)
	cmd.Dir = spec.Dir
	cmd.Env = env
	if err := configureSandbox(cmd, spec.Dir, sandbox); err != nil {
		return nil, fmt.Errorf("runner: terminal sandbox: %w", err)
	}
	return cmd, nil
}
