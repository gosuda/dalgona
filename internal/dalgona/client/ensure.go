package client

import (
	"context"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"time"
)

var (
	ensureTimeout = 5 * time.Second
	ensureRetry   = 50 * time.Millisecond
)

func Ensure(ctx context.Context, home, exe string) (net.Conn, error) {
	if limitErr := CheckSocketPathLimit(home); limitErr != nil {
		return nil, limitErr
	}
	if err := tightenHomeRoot(home); err != nil {
		return nil, err
	}
	if err := VerifyHome(home); err != nil {
		return nil, err
	}
	sock := SocketPath(home)
	if conn, err := reuseSocket(sock); err != nil || conn != nil {
		return conn, err
	}
	if err := spawn(home, exe); err != nil {
		return nil, err
	}
	return waitSocket(ctx, home, sock)
}

func reuseSocket(sock string) (net.Conn, error) {
	ok, err := socketOwned(sock)
	if err != nil {
		return nil, err
	}
	if !ok {
		return nil, nil
	}
	conn, err := dialPeer(sock)
	if err != nil {
		if errors.Is(err, errUntrusted) {
			return nil, err
		}
		return nil, nil
	}
	return conn, nil
}

func socketOwned(sock string) (bool, error) {
	if _, err := os.Lstat(sock); err != nil {
		if os.IsNotExist(err) {
			return false, nil
		}
		return false, fmt.Errorf("daemon socket: %w", err)
	}
	return true, verifySocket(sock)
}

func dialPeer(sock string) (net.Conn, error) {
	conn, err := net.DialTimeout("unix", sock, 200*time.Millisecond)
	if err != nil {
		return nil, err
	}
	if perr := verifyDialedPeer(conn); perr != nil {
		_ = conn.Close()
		return nil, perr
	}
	return annotateSandboxedClient(conn), nil
}

func waitSocket(ctx context.Context, home, sock string) (net.Conn, error) {
	deadline := time.Now().Add(ensureTimeout)
	for {
		if conn, err := reuseSocket(sock); err != nil || conn != nil {
			return conn, err
		}
		if time.Now().After(deadline) {
			return nil, fmt.Errorf("daemon did not start: see %s", filepath.Join(home, "daemon.log"))
		}
		t := time.NewTimer(ensureRetry)
		select {
		case <-ctx.Done():
			t.Stop()
			return nil, context.Cause(ctx)
		case <-t.C:
		}
	}
}
