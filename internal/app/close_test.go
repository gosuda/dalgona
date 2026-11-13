package app

import (
	"context"
	"io"
	"net"
	"path/filepath"
	"testing"
)

func TestInteractiveEndsCleanlyWhenTUIClosedTheConnection(t *testing.T) {
	listener, err := net.Listen("unix", filepath.Join(t.TempDir(), "daemon.sock"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	conn, err := net.Dial("unix", listener.Addr().String())
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
