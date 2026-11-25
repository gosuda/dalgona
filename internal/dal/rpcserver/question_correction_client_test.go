package rpcserver

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"net"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type correctingClient struct {
	answer     jsontext.Value
	requests   chan string
	responders chan func(any) error
	warnings   chan map[string]string
	resolved   chan rpc.ServerRequestResolvedNotification
}

func newCorrectingClient(answer string) *correctingClient {
	return &correctingClient{
		answer: jsontext.Value(answer), requests: make(chan string, 16),
		responders: make(chan func(any) error, 16),
		warnings:   make(chan map[string]string, 16), resolved: make(chan rpc.ServerRequestResolvedNotification, 16),
	}
}

func (c *correctingClient) Notification(method string, params jsontext.Value) {
	var event rpc.EventParams
	if method != rpc.MethodEvent || json.Unmarshal(params, &event) != nil {
		return
	}
	switch event.Method {
	case "warning":
		var warning map[string]string
		if json.Unmarshal(event.Params, &warning) == nil {
			c.warnings <- warning
		}
	case "serverRequest/resolved":
		var resolved rpc.ServerRequestResolvedNotification
		if json.Unmarshal(event.Params, &resolved) == nil {
			c.resolved <- resolved
		}
	}
}

func (c *correctingClient) Request(ctx context.Context, _ string, params jsontext.Value) (any, error) {
	var header rpc.RequestParams
	if err := json.Unmarshal(params, &header); err != nil {
		return nil, err
	}
	c.requests <- header.Details.RequestID
	respond := rpc.ClientResponder(ctx)
	c.responders <- respond
	if err := respond(c.answer); err != nil {
		return nil, err
	}
	return nil, rpc.ErrResponded
}

func within[T any](t *testing.T, ch <-chan T, what string) T {
	t.Helper()
	select {
	case v := <-ch:
		return v
	case <-time.After(waitLimit):
		t.Fatalf("no %s", what)
		var zero T
		return zero
	}
}

func dialCorrectingClient(t *testing.T, r *rig, observer *correctingClient) *rpc.Conn {
	t.Helper()
	near, far := net.Pipe()
	served := make(chan error, 1)
	go func() { served <- ServeWith(t.Context(), far, r.host, ServeOptions{}) }()
	conn := rpc.Dial(t.Context(), near, observer)
	t.Cleanup(func() {
		_ = conn.Close()
		if err := within(t, served, "server return after the client closed"); err != nil {
			t.Logf("serve ended with %v", err)
		}
	})
	var init rpc.InitializeResult
	if err := conn.Call(t.Context(), "initialize", nativeInitialize(rpc.NativeVersion, rpc.NativeDelivery), &init); err != nil || init.DalgonaNative == nil {
		t.Fatalf("native initialize = %+v, %v", init, err)
	}
	return conn
}

func resolutionOf(t *testing.T, observer *correctingClient, requestID string) string {
	t.Helper()
	for {
		if resolved := within(t, observer.resolved, "resolution of "+requestID); resolved.RequestID == requestID {
			return resolved.Resolution
		}
	}
}

func TestDialedClientCorrectsARejectedAnswerThroughResponder(t *testing.T) {
	r := newDalgonaRig(t)
	args := `{"questions":[{"header":"Database","question":"Which database?","options":[{"label":"SQLite","description":"One file."},{"label":"Postgres","description":"A server."}]}]}`
	r.llm.push(llmReply{body: sseCall("call-q", "ask", args)}, llmReply{body: sseText("done")})
	observer := newCorrectingClient(`{"answers":{"q1":{"answers":["bad\u0000text"]}}}`)
	conn := dialCorrectingClient(t, r, observer)
	var started rpc.ThreadStartResponse
	if err := conn.Call(t.Context(), "thread/start", map[string]any{"cwd": r.cwd}, &started); err != nil {
		t.Fatal(err)
	}
	if err := conn.Call(t.Context(), "turn/start", prompt(started.Thread.ID, "ask me"), nil); err != nil {
		t.Fatal(err)
	}
	requestID := within(t, observer.requests, "question request")
	respond := within(t, observer.responders, "client responder")
	if warning := within(t, observer.warnings, "correction warning"); warning["requestId"] != requestID || warning["field"] != "answers.q1.answers[0]" {
		t.Fatalf("warning %v, want a field error for request %s", warning, requestID)
	}
	if err := respond(jsontext.Value(`{"answers":{"q1":{"answers":["Postgres"]}}}`)); err != nil {
		t.Fatal(err)
	}
	if resolution := resolutionOf(t, observer, requestID); resolution != "answered" {
		t.Fatalf("request %s resolved as %q, want answered", requestID, resolution)
	}
}
