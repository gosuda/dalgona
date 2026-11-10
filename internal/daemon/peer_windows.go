//go:build windows

package daemon

import (
	"net"

	"github.com/gosuda/dalgona/internal/auth"
)

func verifyAcceptedPeer(net.Conn, bool) error {
	return nil
}

func verifyDialedPeer(net.Conn) error {
	return nil
}

func verifyHome(home string) error {
	return auth.CheckHomeDir(home)
}

func verifySocket(string) error {
	return nil
}

func daemonMarked() bool { return false }

func logConfinedDaemon(bool) {}

func annotateSandboxedClient(conn net.Conn) net.Conn { return conn }
