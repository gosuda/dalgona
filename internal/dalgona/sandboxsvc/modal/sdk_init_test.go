package modal

import (
	"context"
	"errors"
	"net"
	"testing"
	"time"
)

func blackhole(t *testing.T) string {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = ln.Close() })
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			t.Cleanup(func() { _ = conn.Close() })
			go hold(conn)
		}
	}()
	return ln.Addr().String()
}

func hold(conn net.Conn) {
	_ = conn.SetReadDeadline(time.Now().Add(30 * time.Second))
	var one [1]byte
	_, _ = conn.Read(one[:])
}

func TestNewFromEnvWithImageCtxCancelsBlockedDial(t *testing.T) {
	addr := blackhole(t)
	t.Setenv("MODAL_SERVER_URL", "https://"+addr+",https://"+addr)
	t.Setenv("MODAL_TOKEN_ID", "test-id")
	t.Setenv("MODAL_TOKEN_SECRET", "test-secret")

	ctx, cancel := context.WithCancel(t.Context())
	time.AfterFunc(200*time.Millisecond, cancel)
	start := time.Now()
	_, err := NewFromEnvWithImageCtx(ctx, "test-image", nil)
	elapsed := time.Since(start)
	if !errors.Is(err, ErrInitCancelled) {
		t.Fatalf("constructor error = %v, want %v", err, ErrInitCancelled)
	}
	if elapsed > 10*time.Second {
		t.Fatalf("constructor returned after %v, want a prompt return on cancel", elapsed)
	}
}
