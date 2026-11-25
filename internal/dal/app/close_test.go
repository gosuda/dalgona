package app

import (
	"net"
	"testing"
)

func TestCloseQuietAcceptsAlreadyClosedNetworkConnection(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	conn, err := net.Dial("tcp", listener.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	if err := conn.Close(); err != nil {
		t.Fatal(err)
	}
	if err := closeQuiet(conn); err != nil {
		t.Fatalf("close an already closed connection: %v", err)
	}
}
