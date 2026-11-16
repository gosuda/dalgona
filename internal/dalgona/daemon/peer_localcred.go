//go:build darwin || freebsd

package daemon

import (
	"net"

	"golang.org/x/sys/unix"
)

func peerUIDFromFD(fd int) (int, error) {
	xucred, err := unix.GetsockoptXucred(fd, unix.SOL_LOCAL, unix.LOCAL_PEERCRED)
	if err != nil {
		return -1, err
	}
	return int(xucred.Uid), nil
}

func peerCredsFromFD(fd int) (int, int, error) {
	uid, err := peerUIDFromFD(fd)
	return uid, 0, err
}

func daemonMarked() bool { return false }

func checkPeerSandbox(int, int, bool) error { return nil }

func logConfinedDaemon(bool) {}

func annotateSandboxedClient(conn net.Conn) net.Conn { return conn }
