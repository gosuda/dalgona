package webui

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"strings"
	"sync/atomic"
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

func (r *running) bearerRPC(ctx context.Context, rawQuery, authorization string) (*websocket.Conn, *http.Response, error) {
	target := "ws" + strings.TrimPrefix(r.base, "http") + "/rpc" + rawQuery
	return websocket.Dial(ctx, target, &websocket.DialOptions{HTTPHeader: http.Header{"Authorization": {authorization}}})
}

func TestRPCAcceptsTheTokenAsABearerHeader(t *testing.T) {
	for _, scheme := range []string{"Bearer", "bearer", "BEARER"} {
		t.Run(scheme, func(t *testing.T) {
			daemons := make(chan net.Conn, 1)
			r := start(t, Config{Listen: "127.0.0.1:0"}, builtSite(), pipeDial(daemons))
			ctx, cancel := context.WithTimeout(t.Context(), failureDeadline)
			defer cancel()
			browser, _, err := r.bearerRPC(ctx, "", scheme+" "+r.token)
			if err != nil {
				t.Fatalf("dial rpc with a bearer header: %v", err)
			}
			defer func() { _ = browser.CloseNow() }()
			daemon := <-daemons
			if err := browser.Write(ctx, websocket.MessageText, []byte(`{"id":1}`)); err != nil {
				t.Fatalf("browser write: %v", err)
			}
			got := make([]byte, len(`{"id":1}`)+1)
			if _, err := io.ReadFull(daemon, got); err != nil || string(got) != "{\"id\":1}\n" {
				t.Fatalf("daemon read %q, %v", got, err)
			}
		})
	}
}

func TestRPCRejectsAWrongBearerHeaderWithoutEchoingIt(t *testing.T) {
	var dialed atomic.Int32
	dial := func(context.Context) (net.Conn, error) {
		dialed.Add(1)
		return nil, errors.New("must not dial")
	}
	r := start(t, Config{Listen: "127.0.0.1:0"}, builtSite(), dial)
	wrong := r.token + "x"
	for _, test := range []struct{ name, query, authorization string }{
		{"wrong token", "", "Bearer " + wrong},
		{"empty token", "", "Bearer "},
		{"other scheme", "", "Basic " + r.token},
		{"no scheme", "", r.token},
		{"wrong header beside a right query token", "?token=" + r.token, "Bearer " + wrong},
	} {
		t.Run(test.name, func(t *testing.T) {
			conn, resp, err := r.bearerRPC(t.Context(), test.query, test.authorization)
			if err == nil {
				_ = conn.CloseNow()
				t.Fatalf("authorization %q was accepted", test.authorization)
			}
			if resp == nil || resp.StatusCode != http.StatusUnauthorized {
				t.Fatalf("response %v, want 401", resp)
			}
			body, _ := io.ReadAll(resp.Body)
			if strings.Contains(string(body), wrong) || strings.Contains(string(body), r.token) {
				t.Fatalf("401 body %q echoes a token", body)
			}
		})
	}
	if dialed.Load() != 0 {
		t.Fatalf("daemon dialed %d times for rejected headers", dialed.Load())
	}
}
