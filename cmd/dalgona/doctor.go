package main

import (
	"context"
	"net"
	"slices"
	"time"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/daemon"
	"github.com/gosuda/dalgona/internal/doctor"
)

func runDoctor(ctx context.Context, args []string, std stdio) int {
	online := slices.Equal(args, []string{"--online"})
	if !online && len(args) != 0 {
		return usage(std, "doctor [--online]")
	}
	home := config.ProductDalgona.Home()
	return doctor.Run(ctx, doctor.Env{
		Home:   home,
		Dial:   func(context.Context) (net.Conn, error) { return daemon.DialHome(home) },
		Own:    daemon.Build(),
		Now:    time.Now,
		Out:    std.out,
		Online: online,
	})
}
