package daemon

import (
	"errors"
	"net"
	"os"
	"testing"
)

func TestCheckUIDRejectsMismatch(t *testing.T) {
	self := os.Getuid()
	if err := checkUID(self, self); err != nil {
		t.Fatalf("matching uid refused: %v", err)
	}
	if err := checkUID(self+1, self); !errors.Is(err, errUntrusted) {
		t.Fatalf("mismatching uid accepted: %v", err)
	}
}

func TestVerifyPeerConnSameUID(t *testing.T) {
	home := shortHome(t)
	sock := SocketPath(home)
	listener, err := net.Listen("unix", sock)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	client, err := net.Dial("unix", sock)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = client.Close() })
	server, err := listener.Accept()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = server.Close() })
	if perr := verifyDialedPeer(client); perr != nil {
		t.Fatalf("client peer refused: %v", perr)
	}
	if perr := verifyAcceptedPeer(server, daemonMarked()); perr != nil {
		t.Fatalf("server peer refused: %v", perr)
	}
}
