package app

import (
	"context"
	"io"
	"net"
	"testing"
)

func TestInteractiveEndsCleanlyWhenTUIClosedTheConnection(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	conn, err := net.Dial("tcp", listener.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	env := CLIEnv{
		Dial: func(context.Context) (io.ReadWriteCloser, error) { return conn, nil },
		TUI: func(_ context.Context, rwc io.ReadWriteCloser, _ TUIOptions) error {
			return rwc.Close()
		},
	}
	if err := env.interactive(t.Context(), cliFlags{}); err != nil {
		t.Fatalf("interactive after the TUI closed its connection: %v", err)
	}
}
