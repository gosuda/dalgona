package webui

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
)

const failureDeadline = 10 * time.Second

type relaySend struct {
	ctx       context.Context
	browser   *websocket.Conn
	message   []byte
	writeErr  error
	forwarded chan []byte
}

func sendThroughRelay(t *testing.T, size int) *relaySend {
	t.Helper()
	_, browser, daemon := openRPC(t)
	ctx, cancel := context.WithTimeout(t.Context(), failureDeadline)
	t.Cleanup(cancel)
	s := &relaySend{ctx: ctx, browser: browser, message: bytes.Repeat([]byte{'a'}, size), forwarded: make(chan []byte, 1)}
	go func() {
		data, _ := io.ReadAll(daemon)
		s.forwarded <- data
	}()
	s.writeErr = browser.Write(ctx, websocket.MessageText, s.message)
	return s
}

func (s *relaySend) daemonBytes(t *testing.T) []byte {
	t.Helper()
	select {
	case data := <-s.forwarded:
		return data
	case <-s.ctx.Done():
		t.Fatal("daemon connection stayed open")
		return nil
	}
}

func TestRelayClosesOnOversizeMessage(t *testing.T) {
	t.Run("a browser message of 16 MiB minus 1 byte is relayed", func(t *testing.T) {
		s := sendThroughRelay(t, 16<<20-1)
		if s.writeErr != nil {
			t.Fatalf("browser write: %v", s.writeErr)
		}
		_ = s.browser.Close(websocket.StatusNormalClosure, "")
		if got := s.daemonBytes(t); !bytes.Equal(got, append(s.message, '\n')) {
			t.Fatalf("daemon received %d bytes, want the %d byte message and one newline", len(got), len(s.message))
		}
	})
	t.Run("a browser message of 16 MiB closes the websocket", func(t *testing.T) {
		s := sendThroughRelay(t, 16<<20)
		if _, _, err := s.browser.Read(s.ctx); err == nil || s.ctx.Err() != nil {
			t.Fatalf("browser read: %v (context %v), want the websocket closed", err, s.ctx.Err())
		}
		if got := s.daemonBytes(t); len(got) != 0 {
			t.Fatalf("daemon received %d bytes of an over-size message", len(got))
		}
	})
}

type failingConn struct {
	net.Conn
	readErr  error
	writeErr error
}

func (c failingConn) Read(p []byte) (int, error) {
	if c.readErr != nil {
		return 0, c.readErr
	}
	return c.Conn.Read(p)
}

func (c failingConn) Write(p []byte) (int, error) {
	if c.writeErr != nil {
		return 0, c.writeErr
	}
	return c.Conn.Write(p)
}

func openFailingRelay(t *testing.T, failing func(net.Conn) net.Conn) (*running, *websocket.Conn, context.Context) {
	t.Helper()
	dial := func(context.Context) (net.Conn, error) {
		web, daemon := net.Pipe()
		t.Cleanup(func() { _ = daemon.Close() })
		return failing(web), nil
	}
	r := start(t, Config{Listen: "127.0.0.1:0"}, builtSite(), dial)
	ctx, cancel := context.WithTimeout(t.Context(), failureDeadline)
	t.Cleanup(cancel)
	browser, _, err := websocket.Dial(ctx, r.rpcURL(r.token), nil)
	if err != nil {
		t.Fatalf("dial rpc: %v", err)
	}
	t.Cleanup(func() { _ = browser.CloseNow() })
	return r, browser, ctx
}

func requireInternalErrorClose(ctx context.Context, t *testing.T, r *running, browser *websocket.Conn, failure error) {
	t.Helper()
	if _, _, err := browser.Read(ctx); websocket.CloseStatus(err) != websocket.StatusInternalError {
		t.Fatalf("close %v, want status %v", err, websocket.StatusInternalError)
	}
	select {
	case line := <-r.lines:
		if !strings.HasPrefix(line, "web: rpc relay: ") || !strings.Contains(line, failure.Error()) || !strings.HasSuffix(line, "\n") {
			t.Fatalf("logged %q, want a line that starts with %q and holds %q", line, "web: rpc relay: ", failure)
		}
	case <-ctx.Done():
		t.Fatal("relay failure was not logged")
	}
}

func TestRelayClosesWith1011OnRelayError(t *testing.T) {
	t.Run("the daemon read fails", func(t *testing.T) {
		failure := errors.New("daemon read failed")
		r, browser, ctx := openFailingRelay(t, func(c net.Conn) net.Conn { return failingConn{Conn: c, readErr: failure} })
		requireInternalErrorClose(ctx, t, r, browser, failure)
	})
	t.Run("the daemon write fails", func(t *testing.T) {
		failure := errors.New("daemon write failed")
		r, browser, ctx := openFailingRelay(t, func(c net.Conn) net.Conn { return failingConn{Conn: c, writeErr: failure} })
		if err := browser.Write(ctx, websocket.MessageText, []byte(`{"id":1}`)); err != nil {
			t.Fatalf("browser write: %v", err)
		}
		requireInternalErrorClose(ctx, t, r, browser, failure)
	})
}
