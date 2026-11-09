package tuiwire

import (
	"context"
	"io"

	"github.com/gosuda/dalgona/internal/app"
	"github.com/gosuda/dalgona/internal/tui"
)

func Run(ctx context.Context, conn io.ReadWriteCloser, opts app.TUIOptions) error {
	var onStatus func(tui.Status)
	if opts.OnStatus != nil {
		onStatus = func(s tui.Status) {
			opts.OnStatus(app.Status{State: s.State, Seq: s.Seq, ThreadID: s.ThreadID})
		}
	}
	return tui.Run(ctx, conn, tui.Options{
		OnStatus: onStatus, OnChildSpawned: opts.OnChildSpawned, Input: opts.Stdin, Output: opts.Stdout,
		ThreadID: opts.ThreadID, Model: opts.Model, Approval: opts.Approval, Sandbox: opts.Sandbox,
	})
}
