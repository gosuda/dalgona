package rpc

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"net"
	"sync"
	"testing"
	"time"
)

type dispatchProbe struct {
	mu      sync.Mutex
	methods []string
	entered chan struct{}
	release chan struct{}
	reqCtx  context.Context
}

func (h *dispatchProbe) Notification(method string, params jsontext.Value) {
	h.mu.Lock()
	h.methods = append(h.methods, method)
	h.mu.Unlock()
}

func (h *dispatchProbe) Request(ctx context.Context, method string, params jsontext.Value) (any, error) {
	h.mu.Lock()
	h.reqCtx = ctx
	h.mu.Unlock()
	close(h.entered)
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-h.release:
		return map[string]string{"decision": "accept"}, nil
	}
}

func (h *dispatchProbe) saw(method string) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	for _, m := range h.methods {
		if m == method {
			return true
		}
	}
	return false
}

func (h *dispatchProbe) ctxErr() error {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.reqCtx == nil {
		return nil
	}
	select {
	case <-h.reqCtx.Done():
		return h.reqCtx.Err()
	default:
		return nil
	}
}

func writeLine(t *testing.T, c net.Conn, v any) {
	t.Helper()
	line, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := c.Write(append(line, '\n')); err != nil {
		t.Fatal(err)
	}
}

func TestClientServerRequestsDoNotBlockNotifications(t *testing.T) {
	left, right := net.Pipe()
	h := &dispatchProbe{entered: make(chan struct{}), release: make(chan struct{})}
	client, err := Dial(right, h)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = client.Close() })
	writeLine(t, left, envelope{JSONRPC: "2.0", ID: jsontext.Value(`"req-1"`), Method: "item/commandExecution/requestApproval", Params: jsontext.Value(`{"threadId":"t"}`)})
	select {
	case <-h.entered:
	case <-time.After(waitLimit):
		t.Fatal("server request never reached the handler")
	}
	writeLine(t, left, envelope{JSONRPC: "2.0", Method: "turn/started", Params: jsontext.Value(`{}`)})
	writeLine(t, left, envelope{JSONRPC: "2.0", Method: "thread/status/changed", Params: jsontext.Value(`{}`)})
	deadline := time.Now().Add(waitLimit)
	for !h.saw("turn/started") || !h.saw("thread/status/changed") {
		if time.Now().After(deadline) {
			t.Fatal("notifications stopped while a server request was open")
		}
		time.Sleep(10 * time.Millisecond)
	}
	writeLine(t, left, envelope{JSONRPC: "2.0", Method: "serverRequest/resolved", Params: jsontext.Value(`{"threadId":"t","requestId":"req-1"}`)})
	resolved := time.Now().Add(waitLimit)
	for h.ctxErr() == nil {
		if time.Now().After(resolved) {
			t.Fatal("server request context survived its resolved notification")
		}
		time.Sleep(10 * time.Millisecond)
	}
	close(h.release)
}
