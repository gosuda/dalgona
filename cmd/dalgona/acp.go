package main

import (
	"context"
	"io"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/acp"
)

func runACP(ctx context.Context, args []string, std stdio) int {
	if len(args) != 0 {
		return usage(std, "acp")
	}
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	dial, err := daemonDialer(cfg, std)
	if err != nil {
		return fail(std, 1, err)
	}
	opts := acp.Options{
		Version: config.Version(),
		Dial:    func(ctx context.Context) (io.ReadWriteCloser, error) { return dial(ctx) },
	}
	if err = acp.Serve(ctx, std.in, std.out, opts); err != nil {
		return fail(std, 1, err)
	}
	return 0
}
