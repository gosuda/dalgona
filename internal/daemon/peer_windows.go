//go:build windows

package daemon

import "net"

func verifyAcceptedPeer(net.Conn, bool) error {
	return nil
}

func verifyDialedPeer(net.Conn) error {
	return nil
}

func verifyHome(string) error {
	return nil
}

func verifySocket(string) error {
	return nil
}

func daemonMarked() bool { return false }

func logConfinedDaemon(bool) {}

func annotateSandboxedClient(conn net.Conn) net.Conn { return conn }
