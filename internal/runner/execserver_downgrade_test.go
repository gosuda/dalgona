//go:build !windows

package runner

import (
	"errors"
	"net"
	"net/http"
	"strings"
	"testing"

	"github.com/coder/websocket"
)

func TestExecWS_boxChannelRefusesSandboxOff(t *testing.T) {
	t.Setenv("DALGONA_SANDBOX_MODE", string(SandboxWorkspaceWrite))
	local := newLocalContractRunner(t)
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	token := strings.Repeat("downgrade-token-", 3)
	done := make(chan error, 1)
	go func() { done <- serveExecWS(t.Context(), listener, local.(*Local), token, nil) }()
	t.Cleanup(func() { stopContractServer(t, listener, done) })
	header := http.Header{"Authorization": []string{"Bearer " + token}}
	ws, _, err := websocket.Dial(t.Context(), "ws://"+listener.Addr().String()+"/", &websocket.DialOptions{HTTPHeader: header})
	if err != nil {
		t.Fatal(err)
	}
	conn := websocket.NetConn(t.Context(), ws, websocket.MessageText)
	remote, err := NewRemote(t.Context(), "boxtest", conn)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = remote.Close() }()
	err = remote.Start(t.Context(), ProcessSpec{ID: "escape", Argv: []string{"sh", "-c", "echo hi"}})
	if err == nil || !strings.Contains(err.Error(), "sandbox") {
		t.Fatalf("sandbox-off over box channel accepted, err = %v", err)
	}
	if _, err := local.Read(t.Context(), "escape", 0); !errors.Is(err, ErrProcessNotFound) {
		t.Fatalf("downgraded process started, read err = %v", err)
	}
}
