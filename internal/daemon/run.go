package daemon

import (
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"sync"

	"github.com/gosuda/dalgona/internal/agent"
)

type (
	HostFactory func(context.Context) (*agent.Host, func() error, error)
	ServeFunc   func(context.Context, io.ReadWriteCloser, *agent.Host) error
)

func SocketPath(home string) string { return filepath.Join(home, "dalgona.sock") }

func Run(ctx context.Context, home string, newHost HostFactory, serve ServeFunc) (err error) {
	if chmodErr := tightenHome(home); chmodErr != nil {
		return chmodErr
	}
	host, closeHost, err := newHost(ctx)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, closeHost()) }()
	listener, err := listen(home)
	if err != nil {
		return err
	}
	var tasks sync.WaitGroup
	defer tasks.Wait()
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	tasks.Go(func() {
		if recoverErr := host.Recover(ctx); recoverErr != nil {
			slog.Error("daemon: recovery", "error", recoverErr)
		}
	})
	stopListener := context.AfterFunc(ctx, func() { closeListener(listener) })
	defer stopListener()
	return accept(ctx, listener, &tasks, func(conn net.Conn) { serveConn(ctx, conn, host, serve) })
}

func makeHomeDir(home string) error {
	if err := os.MkdirAll(home, 0o700); err != nil {
		return fmt.Errorf("daemon home: %w", err)
	}
	return nil
}

func tightenHome(home string) error {
	if err := makeHomeDir(home); err != nil {
		return err
	}
	if err := os.Chmod(home, 0o700); err != nil { //nolint:gosec // a home directory needs the exec bit
		return fmt.Errorf("daemon home permissions: %w", err)
	}
	return nil
}

func listen(home string) (net.Listener, error) {
	socket := SocketPath(home)
	if err := os.Remove(socket); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return nil, fmt.Errorf("remove stale socket: %w", err)
	}
	listener, err := net.Listen("unix", socket)
	if err != nil {
		return nil, fmt.Errorf("listen on %s: %w", socket, err)
	}
	chmodErr := os.Chmod(socket, 0o600)
	if chmodErr != nil {
		_ = listener.Close()
		return nil, fmt.Errorf("chmod %s: %w", socket, chmodErr)
	}
	return listener, nil
}

func accept(ctx context.Context, listener net.Listener, tasks *sync.WaitGroup, handle func(net.Conn)) error {
	defer closeListener(listener)
	for {
		conn, err := listener.Accept()
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			return fmt.Errorf("daemon accept: %w", err)
		}
		if perr := verifyPeerConn(conn); perr != nil {
			_ = conn.Close()
			slog.Warn("daemon: refusing connection", "error", perr)
			continue
		}
		tasks.Go(func() { handle(conn) })
	}
}

func serveConn(ctx context.Context, conn net.Conn, host *agent.Host, serve ServeFunc) {
	stop := context.AfterFunc(ctx, func() { _ = conn.Close() })
	defer stop()
	if err := serve(ctx, conn, host); err != nil && ctx.Err() == nil {
		slog.Warn("daemon: connection ended", "error", err)
	}
	if err := conn.Close(); err != nil && !errors.Is(err, net.ErrClosed) {
		slog.Warn("daemon: close connection", "error", err)
	}
}

func closeListener(listener net.Listener) {
	if err := listener.Close(); err != nil && !errors.Is(err, net.ErrClosed) {
		slog.Warn("daemon: close listener", "error", err)
	}
}
