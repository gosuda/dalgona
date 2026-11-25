//go:build unix

package runner

import (
	"bytes"
	"context"
	"io"
	"net"
	"os"
	"strings"
	"testing"
	"time"
)

func terminalTestSpec(t *testing.T, columns, rows int) TerminalSpec {
	t.Helper()
	return TerminalSpec{
		Dir:     t.TempDir(),
		Env:     []string{"SHELL=/bin/sh", "PATH=" + os.Getenv("PATH"), "PS1=$ "},
		Sandbox: Sandbox{Mode: SandboxOff},
		Columns: columns,
		Rows:    rows,
	}
}

func readTerminalUntil(t *testing.T, term Terminal, want string) string {
	t.Helper()
	var seen bytes.Buffer
	chunk := make([]byte, 4096)
	deadline := time.AfterFunc(10*time.Second, func() { _ = term.Close() })
	defer deadline.Stop()
	for !strings.Contains(seen.String(), want) {
		n, err := term.Read(chunk)
		seen.Write(chunk[:n])
		if err != nil {
			t.Fatalf("terminal output ended before %q: %v; saw %q", want, err, seen.String())
		}
	}
	return seen.String()
}

func drainTerminal(t *testing.T, term Terminal) string {
	t.Helper()
	deadline := time.AfterFunc(10*time.Second, func() { _ = term.Close() })
	defer deadline.Stop()
	rest, err := io.ReadAll(term)
	if err != nil {
		t.Fatalf("drain terminal: %v", err)
	}
	return string(rest)
}

func exerciseTerminal(t *testing.T, term Terminal) {
	t.Helper()
	t.Cleanup(func() { _ = term.Close() })
	if _, err := term.Write([]byte("stty size\n")); err != nil {
		t.Fatal(err)
	}
	readTerminalUntil(t, term, "30 100")
	if err := term.Resize(120, 40); err != nil {
		t.Fatal(err)
	}
	if _, err := term.Write([]byte("stty size; exit 3\n")); err != nil {
		t.Fatal(err)
	}
	readTerminalUntil(t, term, "40 120")
	drainTerminal(t, term)
	select {
	case <-term.Done():
	case <-time.After(10 * time.Second):
		t.Fatal("terminal shell did not report its exit")
	}
	if exit := term.Exit(); exit.Code != 3 {
		t.Fatalf("exit = %+v, want code 3", exit)
	}
}

func TestLocalTerminalCarriesInputResizeOutputAndExit(t *testing.T) {
	local, err := NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	term, err := local.OpenTerminal(t.Context(), terminalTestSpec(t, 100, 30))
	if err != nil {
		t.Fatal(err)
	}
	exerciseTerminal(t, term)
}

func TestRemoteTerminalCarriesInputResizeOutputAndExit(t *testing.T) {
	local, err := NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	remote := serveTerminalRemote(t, local)
	term, err := remote.OpenTerminal(t.Context(), terminalTestSpec(t, 100, 30))
	if err != nil {
		t.Fatal(err)
	}
	exerciseTerminal(t, term)
}

func TestLocalTerminalKillEndsShellAndOutput(t *testing.T) {
	local, err := NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	term, err := local.OpenTerminal(t.Context(), terminalTestSpec(t, 80, 24))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = term.Close() })
	if _, err := term.Write([]byte("echo ready; sleep 60\n")); err != nil {
		t.Fatal(err)
	}
	readTerminalUntil(t, term, "ready\r\n")
	if err := term.Signal(SignalKill); err != nil {
		t.Fatal(err)
	}
	drainTerminal(t, term)
	if exit := term.Exit(); exit.Signal != "killed" {
		t.Fatalf("exit = %+v, want the shell killed", exit)
	}
}

func serveTerminalRemote(t *testing.T, local *Local) *Remote {
	t.Helper()
	client, server := net.Pipe()
	ctx, cancel := context.WithCancel(context.Background())
	served := make(chan struct{})
	go func() {
		defer close(served)
		_ = serveExecRPC(ctx, server, local, trustedExecPolicy())
	}()
	t.Cleanup(func() {
		cancel()
		_ = client.Close()
		<-served
	})
	remote, err := NewRemote(t.Context(), "terminal-test", client)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = remote.Close() })
	return remote
}
