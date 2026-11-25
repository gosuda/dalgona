package tuiwire

import (
	"context"
	"errors"
	"io"

	"github.com/charmbracelet/x/term"

	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/tui"
)

var errNoTerminal = errors.New("the interactive interface needs a terminal on stdout; use -p <prompt> for one turn")

func Local(dial func(context.Context) (io.ReadWriteCloser, error)) app.TUIBackend {
	return Launch("", tui.Local(dial))
}

func Launch(initial string, open tui.OpenMachines) app.TUIBackend {
	return func(ctx context.Context, opts app.TUIOptions) error {
		return run(ctx, initial, open, opts)
	}
}

func run(ctx context.Context, initial string, open tui.OpenMachines, opts app.TUIOptions) error {
	if !isTerminal(opts.Stdout) {
		return errNoTerminal
	}
	var onStatus func(tui.Status)
	if opts.OnStatus != nil {
		onStatus = func(s tui.Status) {
			opts.OnStatus(app.Status{Endpoint: s.Endpoint, Remote: s.Remote, State: s.State, Seq: s.Seq, ThreadID: s.ThreadID})
		}
	}
	return tui.Run(ctx, open, tui.Options{
		OnStatus: onStatus, OnChildSpawned: opts.OnChildSpawned, Input: opts.Stdin, Output: opts.Stdout,
		ThreadID: opts.ThreadID, Latest: opts.Latest, InitialMachine: initial, Model: opts.Model,
		Approval: opts.Approval, Sandbox: opts.Sandbox, Cwd: opts.Cwd, ExplicitCwd: opts.ExplicitCwd,
	})
}

func isTerminal(out io.Writer) bool {
	file, ok := out.(term.File)
	return ok && term.IsTerminal(file.Fd())
}
