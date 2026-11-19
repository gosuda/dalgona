package tuiwire

import (
	"context"
	"errors"
	"io"
	"os"

	"github.com/charmbracelet/x/term"

	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/tui"
)

var errNoTerminal = errors.New("the interactive interface needs a terminal on stdout; use -p <prompt> for one turn")

func Run(ctx context.Context, conn io.ReadWriteCloser, opts app.TUIOptions) error {
	if !isTerminal(opts.Stdout) {
		return errNoTerminal
	}
	var onStatus func(tui.Status)
	if opts.OnStatus != nil {
		onStatus = func(s tui.Status) {
			opts.OnStatus(app.Status{State: s.State, Seq: s.Seq, ThreadID: s.ThreadID})
		}
	}
	return tui.Run(ctx, conn, tui.Options{
		OnStatus: onStatus, OnChildSpawned: opts.OnChildSpawned, Input: opts.Stdin, Output: opts.Stdout,
		ThreadID: opts.ThreadID, Model: opts.Model, Approval: opts.Approval, Sandbox: opts.Sandbox, Cwd: opts.Cwd,
	})
}

func isTerminal(out io.Writer) bool {
	if out == nil {
		out = os.Stdout
	}
	file, ok := out.(term.File)
	return ok && term.IsTerminal(file.Fd())
}
