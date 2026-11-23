//go:build !windows

package client

import (
	"context"
	"errors"
	"io"
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func listenHome(t *testing.T, home string) {
	t.Helper()
	if err := TightenHome(home); err != nil {
		t.Fatal(err)
	}
	listener, err := net.Listen("unix", SocketPath(home))
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(SocketPath(home), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			go func() { _, _ = io.Copy(io.Discard, conn) }()
		}
	}()
}

func TestSessionCloseReleasesItsConnectionsAndRefusesMore(t *testing.T) {
	home := shortHome(t)
	listenHome(t, home)
	session := Local(home, Backend{Path: filepath.Join(home, "no-backend")})
	conn, err := session.Reconnect(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := conn.(interface{ CloseWrite() error }); !ok {
		t.Fatal("session connection lost CloseWrite")
	}
	if err := session.Close(); err != nil {
		t.Fatal(err)
	}
	_ = conn.SetReadDeadline(time.Now().Add(time.Second))
	if _, err := conn.Read(make([]byte, 1)); !errors.Is(err, net.ErrClosed) {
		t.Fatalf("read after Close = %v, want closed connection", err)
	}
	for name, dial := range map[string]func() (net.Conn, error){
		"connect":   func() (net.Conn, error) { return session.Connect(t.Context()) },
		"reconnect": func() (net.Conn, error) { return session.Reconnect(t.Context()) },
	} {
		if _, err := dial(); !errors.Is(err, ErrSessionClosed) {
			t.Errorf("%s after Close = %v, want ErrSessionClosed", name, err)
		}
	}
	if _, err := os.Lstat(SocketPath(home)); err != nil {
		t.Fatalf("Close touched the daemon socket: %v", err)
	}
}

func TestSessionReconnectNeverStartsTheBackend(t *testing.T) {
	home := shortHome(t)
	if err := TightenHome(home); err != nil {
		t.Fatal(err)
	}
	marker := filepath.Join(home, "started")
	backend := filepath.Join(home, "backend")
	if err := os.WriteFile(backend, []byte("#!/bin/sh\nprintf started > \""+marker+"\"\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	session := Local(home, Backend{Path: backend})
	defer func() { _ = session.Close() }()
	if _, err := session.Reconnect(t.Context()); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("reconnect without a daemon = %v, want missing socket", err)
	}
	time.Sleep(100 * time.Millisecond)
	if _, err := os.Stat(marker); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("reconnect started the backend: %v", err)
	}
}

func TestSessionConnectionCloseForgetsIt(t *testing.T) {
	home := shortHome(t)
	listenHome(t, home)
	session := Local(home, Backend{})
	conn, err := session.Reconnect(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if err := conn.Close(); err != nil {
		t.Fatal(err)
	}
	session.mu.Lock()
	held := len(session.conns)
	session.mu.Unlock()
	if held != 0 {
		t.Fatalf("session holds %d closed connections, want 0", held)
	}
	if err := session.Close(); err != nil {
		t.Fatalf("Close after the caller closed its connection: %v", err)
	}
}

func TestSessionConnectAfterAReconnectOnlyDials(t *testing.T) {
	home := shortHome(t)
	listenHome(t, home)
	session := Local(home, Backend{Build: "never-probed"})
	defer func() { _ = session.Close() }()
	first, err := session.Reconnect(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	_ = first.Close()
	ctx, cancel := context.WithTimeout(t.Context(), 2*time.Second)
	defer cancel()
	second, err := session.Connect(ctx)
	if err != nil {
		t.Fatalf("connect after a successful reconnect probed or launched: %v", err)
	}
	_ = second.Close()
}
