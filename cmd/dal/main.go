package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"sync"
	"sync/atomic"

	"github.com/gosuda/dalgona/cmd/internal/tuiwire"
	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/advisor"
	"github.com/gosuda/dalgona/internal/app"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/rpc"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool/search"
)

const binary = "dal"

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt)
	code := run(ctx, os.Args[1:], os.Stdin, os.Stdout, os.Stderr)
	stop()
	os.Exit(code)
}

func run(ctx context.Context, args []string, stdin io.Reader, stdout, stderr io.Writer) int {
	if err := config.AnchorHome(); err != nil {
		fmt.Fprintf(stderr, "%s: %v\n", binary, err)
		return 1
	}
	if len(args) > 0 {
		switch args[0] {
		case "__shim":
			return runner.ShimMain(args[1:])
		case "__search":
			return search.Main(args[1:], stdout)
		case "__search-index":
			return search.BuildMain(args[1:], stdout)
		case "--version":
			fmt.Fprintf(stdout, "%s %s\n", binary, config.Version())
			return 0
		}
	}
	home := config.ProductDal.Home()
	local := newInProcess(ctx, home)
	env := app.CLIEnv{
		Product: config.ProductDal, Home: home, Dial: local.dial,
		Stdin: stdin, Stdout: stdout, Stderr: stderr,
		TUI: tuiwire.Run,
	}
	code := app.RunCLI(ctx, env, args)
	if err := local.Close(); err != nil {
		fmt.Fprintf(stderr, "%s: close: %v\n", binary, err)
		return max(code, 1)
	}
	return code
}

type servedPipe struct {
	net.Conn
	closeOnce func() error
}

func (p *servedPipe) Close() error { return p.closeOnce() }

func newServedPipe(conn net.Conn, served <-chan error) *servedPipe {
	return &servedPipe{Conn: conn, closeOnce: sync.OnceValue(func() error { return errors.Join(conn.Close(), <-served) })}
}

type localHost struct {
	host     *agent.Host
	log      *slog.Logger
	advisors *advisor.Registry
	close    func() error
}

type inProcess struct {
	open    func() (localHost, error)
	started atomic.Pointer[localHost]
}

func newInProcess(ctx context.Context, home string) *inProcess {
	p := &inProcess{}
	p.open = sync.OnceValues(func() (localHost, error) {
		local, err := startHost(ctx, home)
		if err == nil {
			p.started.Store(&local)
		}
		return local, err
	})
	return p
}

func startHost(ctx context.Context, home string) (localHost, error) {
	cwd, err := os.Getwd()
	if err != nil {
		return localHost{}, err
	}
	logger, closeLog, err := openLog(home)
	if err != nil {
		return localHost{}, err
	}
	advisors := advisor.NewRegistry()
	host, closeHost, err := app.NewHost(ctx, app.Options{Product: config.ProductDal, Cwd: cwd, Logger: logger, Advisors: advisors, Batteries: evalBatteries})
	if err != nil {
		return localHost{}, errors.Join(err, closeLog())
	}
	closeAll := func() error { return errors.Join(closeHost(), closeLog()) }
	return localHost{host: host, log: logger, advisors: advisors, close: closeAll}, nil
}

func (p *inProcess) dial(ctx context.Context) (io.ReadWriteCloser, error) {
	local, err := p.open()
	if err != nil {
		return nil, err
	}
	client, server := net.Pipe()
	served := make(chan error, 1)
	go func() {
		options := rpc.ServeOptions{Logger: local.log, Advisors: local.advisors}
		served <- rpc.ServeWith(context.WithoutCancel(ctx), server, local.host, options)
	}()
	return newServedPipe(client, served), nil
}

func (p *inProcess) Close() error {
	if local := p.started.Load(); local != nil {
		return local.close()
	}
	return nil
}

func openLog(home string) (*slog.Logger, func() error, error) {
	if homeErr := auth.ProtectHomeTree(home); homeErr != nil {
		return nil, nil, fmt.Errorf("create %s: %w", home, homeErr)
	}
	file, err := os.OpenFile(filepath.Join(home, "dal.log"), os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
	if err != nil {
		return nil, nil, fmt.Errorf("open dal.log: %w", err)
	}
	return slog.New(slog.NewTextHandler(file, &slog.HandlerOptions{Level: slog.LevelInfo})), file.Close, nil
}
