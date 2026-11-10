//go:build windows

package daemon

import "net"

func verifyPeerConn(net.Conn) error {
	return nil
}

func verifyHome(string) error {
	return nil
}

func verifySocket(string) error {
	return nil
}
