//go:build unix && !linux && !darwin && !freebsd

package client

import (
	"fmt"
	"net"
)

func peerUIDFromFD(int) (int, error) {
	return -1, fmt.Errorf("%w: peer check unsupported", errUntrusted)
}

func peerCredsFromFD(fd int) (int, int, error) {
	uid, err := peerUIDFromFD(fd)
	return uid, 0, err
}

func DaemonMarked() bool { return false }

func checkPeerSandbox(int, int, bool) error { return nil }

func LogConfinedDaemon(bool) {}

func annotateSandboxedClient(conn net.Conn) net.Conn { return conn }
