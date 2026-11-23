//go:build windows

package client

import (
	"net"

	"github.com/gosuda/dalgona/internal/dal/auth"
)

func VerifyAcceptedPeer(net.Conn, bool) error {
	return nil
}

func verifyDialedPeer(net.Conn) error {
	return nil
}

func VerifyHome(home string) error {
	return auth.CheckHomeDir(home)
}

func verifySocket(string) error {
	return nil
}

func DaemonMarked() bool { return false }

func LogConfinedDaemon(bool) {}

func annotateSandboxedClient(conn net.Conn) net.Conn { return conn }
