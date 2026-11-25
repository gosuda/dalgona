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
	"sync"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

type (
	HostFactory   func(context.Context) (*agent.Host, func() error, error)
	ServeFunc     func(context.Context, io.ReadWriteCloser, *agent.Host) error
	RecoveredFunc func(context.Context, *agent.Host, error)
)

var listenFunc = listen

func Run(ctx context.Context, home string, newHost HostFactory, serve ServeFunc, after ...RecoveredFunc) (err error) {
	if limitErr := client.CheckSocketPathLimit(home); limitErr != nil {
		return limitErr
	}
	if chmodErr := client.TightenHome(home); chmodErr != nil {
		return chmodErr
	}
	marked := client.DaemonMarked()
	client.LogConfinedDaemon(marked)
	host, closeHost, err := newHost(ctx)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, closeHost(), removeSocket(home)) }()
	listener, err := listenFunc(home)
	if err != nil {
		return err
	}
	var tasks sync.WaitGroup
	defer tasks.Wait()
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	tasks.Go(func() {
		recoverErr := host.Recover(ctx)
		if recoverErr != nil {
			slog.Error("daemon: recovery", "error", recoverErr)
		}
		for _, fn := range after {
			fn(ctx, host, recoverErr)
		}
	})
	stopListener := context.AfterFunc(ctx, func() { closeListener(listener) })
	defer stopListener()
	return accept(ctx, listener, &tasks, marked, func(conn net.Conn) { serveConn(ctx, conn, host, serve) })
}

func listen(home string) (net.Listener, error) {
	socket := client.SocketPath(home)
	if err := os.Remove(socket); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return nil, fmt.Errorf("remove stale socket: %w", err)
	}
	listener, err := net.Listen("unix", socket)
	if err != nil {
		return nil, fmt.Errorf("listen on %s: %w", socket, err)
	}
	if unixListener, ok := listener.(*net.UnixListener); ok {
		unixListener.SetUnlinkOnClose(false)
	}
	chmodErr := os.Chmod(socket, 0o600)
	if chmodErr != nil {
		_ = listener.Close()
		_ = os.Remove(socket)
		return nil, fmt.Errorf("chmod %s: %w", socket, chmodErr)
	}
	return listener, nil
}

func removeSocket(home string) error {
	if err := os.Remove(client.SocketPath(home)); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	return nil
}

func accept(ctx context.Context, listener net.Listener, tasks *sync.WaitGroup, marked bool, handle func(net.Conn)) error {
	defer closeListener(listener)
	for {
		conn, err := listener.Accept()
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			return fmt.Errorf("daemon accept: %w", err)
		}
		if perr := client.VerifyAcceptedPeer(conn, marked); perr != nil {
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
