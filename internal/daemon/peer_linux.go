//go:build linux

package daemon

import (
	"errors"
	"fmt"
	"log/slog"
	"net"
	"os"
	"strconv"
	"strings"
	"sync/atomic"

	"golang.org/x/sys/unix"
)

var errSandboxed = errors.New("daemon: refusing sandboxed peer")

func peerUIDFromFD(fd int) (int, error) {
	uid, _, err := peerCredsFromFD(fd)
	return uid, err
}

func peerCredsFromFD(fd int) (int, int, error) {
	ucred, err := unix.GetsockoptUcred(fd, unix.SOL_SOCKET, unix.SO_PEERCRED)
	if err != nil {
		return -1, 0, err
	}
	return int(ucred.Uid), int(ucred.Pid), nil
}

func daemonMarked() bool {
	marked, err := peerSelfNoNewPrivs()
	return err == nil && marked
}

func checkPeerSandbox(fd int, pid int, daemonMarked bool) error {
	peerSet, peerErr := peerNoNewPrivs(fd, pid)
	return refuseSandboxedPeer(sandboxMarks{peer: peerSet, daemon: daemonMarked, readErr: peerErr})
}

type sandboxMarks struct {
	peer    bool
	daemon  bool
	readErr error
}

func refuseSandboxedPeer(marks sandboxMarks) error {
	if marks.readErr != nil {
		return marks.readErr
	}
	if marks.daemon {
		return nil
	}
	if marks.peer {
		return fmt.Errorf("%w: peer is inside a sandbox", errSandboxed)
	}
	return nil
}

func peerSelfNoNewPrivs() (bool, error) {
	set, err := unix.PrctlRetInt(unix.PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0)
	if err != nil {
		return false, fmt.Errorf("daemon: read no_new_privs: %w", err)
	}
	return set != 0, nil
}

func peerNoNewPrivs(fd int, pid int) (bool, error) {
	pidfd, err := unix.GetsockoptInt(fd, unix.SOL_SOCKET, unix.SO_PEERPIDFD)
	if err != nil {
		if errors.Is(err, unix.ENOPROTOOPT) {
			return noNewPrivsPid(pid)
		}
		return false, fmt.Errorf("daemon: read peer pidfd: %w", err)
	}
	defer func() { _ = unix.Close(pidfd) }()
	set, readErr := noNewPrivsPid(pid)
	if readErr != nil {
		return false, readErr
	}
	if sigErr := unix.PidfdSendSignal(pidfd, 0, nil, 0); sigErr != nil {
		return false, fmt.Errorf("daemon: peer changed during check: %w", sigErr)
	}
	return set, nil
}

func noNewPrivsPid(pid int) (bool, error) {
	data, err := os.ReadFile("/proc/" + strconv.Itoa(pid) + "/status")
	if err != nil {
		return false, fmt.Errorf("daemon: read peer status: %w", err)
	}
	for line := range strings.SplitSeq(string(data), "\n") {
		name, value, ok := strings.Cut(line, ":")
		if !ok || strings.TrimSpace(name) != "NoNewPrivs" {
			continue
		}
		return strings.TrimSpace(value) != "0", nil
	}
	return false, errors.New("daemon: peer status hides no_new_privs")
}

var errSandboxedClient = errors.New("daemon: daemon refuses clients that run inside a sandbox")

func logConfinedDaemon(daemonMarked bool) {
	if !daemonMarked {
		return
	}
	slog.Warn("daemon: running with no_new_privs; sandboxed commands can reach the daemon socket")
}

func annotateSandboxedClient(conn net.Conn) net.Conn {
	return &sandboxedClientConn{Conn: conn}
}

type sandboxedClientConn struct {
	net.Conn
	noted atomic.Bool
}

func (c *sandboxedClientConn) Read(b []byte) (int, error) {
	n, err := c.Conn.Read(b)
	if err != nil {
		return n, noteSandboxedClient(err, &c.noted)
	}
	return n, nil
}

func (c *sandboxedClientConn) Write(b []byte) (int, error) {
	n, err := c.Conn.Write(b)
	if err != nil {
		return n, noteSandboxedClient(err, &c.noted)
	}
	return n, nil
}

func noteSandboxedClient(err error, noted *atomic.Bool) error {
	if noted.Swap(true) {
		return err
	}
	marked, markErr := peerSelfNoNewPrivs()
	if markErr != nil || !marked {
		return err
	}
	return fmt.Errorf("%w: this process runs inside a sandbox: %w", errSandboxedClient, err)
}
