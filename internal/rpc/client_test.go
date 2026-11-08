package rpc

import (
	"bufio"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"io"
	"net"
	"testing"
	"time"
)

type orderedClientHandler struct {
	events        []int
	requestMethod string
	err           error
	done          chan struct{}
}

type clientServerOutcome struct {
	response envelope
	err      error
}

func TestDialDeliversNotificationsAndRequestInOrder(t *testing.T) {
	left, right := net.Pipe()
	handler := &orderedClientHandler{events: make([]int, 0, 1001), done: make(chan struct{})}
	client, err := Dial(left, handler)
	if err != nil {
		t.Fatal(err)
	}
	closeRPCPair(t, client, right)
	outcome := make(chan clientServerOutcome, 1)
	go func() { outcome <- runClientServer(right) }()

	result := waitClientOutcome(t, outcome)
	waitClientRequest(t, handler)
	requireClientEvents(t, handler)
	requireClientResponse(t, result.response)
}

func waitClientOutcome(t *testing.T, outcome <-chan clientServerOutcome) clientServerOutcome {
	t.Helper()
	select {
	case result := <-outcome:
		if result.err != nil {
			t.Fatal(result.err)
		}
		return result
	case <-time.After(5 * time.Second):
		t.Fatal("client server did not finish")
		return clientServerOutcome{}
	}
}

func waitClientRequest(t *testing.T, handler *orderedClientHandler) {
	t.Helper()
	select {
	case <-handler.done:
	case <-time.After(time.Second):
		t.Fatal("client request was not handled")
	}
	if handler.err != nil {
		t.Fatal(handler.err)
	}
}

func requireClientEvents(t *testing.T, handler *orderedClientHandler) {
	t.Helper()
	if handler.requestMethod != "item/commandExecution/requestApproval" {
		t.Fatalf("request method=%q", handler.requestMethod)
	}
	if len(handler.events) != 1001 {
		t.Fatalf("events=%d, want 1001", len(handler.events))
	}
	for i, event := range handler.events {
		if event != i {
			t.Fatalf("event %d=%d", i, event)
		}
	}
}

func requireClientResponse(t *testing.T, result envelope) {
	t.Helper()
	var response ApprovalResponse
	if err := json.Unmarshal(result.Result, &response); err != nil {
		t.Fatal(err)
	}
	if response.Decision != "accept" || result.ID.String() != "1" {
		t.Fatalf("response=%s, id=%s", result.Result, result.ID)
	}
}

func (h *orderedClientHandler) Notification(method string, params jsontext.Value) {
	if method != "test/event" {
		h.err = fmt.Errorf("notification method=%q", method)
		return
	}
	n, err := clientEventNumber(params)
	if err != nil {
		h.err = err
		return
	}
	h.events = append(h.events, n)
}

func (h *orderedClientHandler) Request(_ context.Context, method string, params jsontext.Value) (any, error) {
	defer close(h.done)
	h.requestMethod = method
	n, err := clientEventNumber(params)
	if err != nil {
		return nil, err
	}
	h.events = append(h.events, n)
	return ApprovalResponse{Decision: "accept"}, nil
}

func clientEventNumber(params jsontext.Value) (int, error) {
	var event struct {
		N int `json:"n"`
	}
	if err := json.Unmarshal(params, &event); err != nil {
		return 0, fmt.Errorf("decode event: %w", err)
	}
	return event.N, nil
}

func runClientServer(conn net.Conn) clientServerOutcome {
	for i := range 1000 {
		params := jsontext.Value(fmt.Sprintf(`{"n":%d}`, i))
		message := envelope{JSONRPC: "2.0", Method: "test/event", Params: params}
		if err := writeClientMessage(conn, message); err != nil {
			return clientServerOutcome{err: err}
		}
	}
	params := jsontext.Value(`{"n":1000}`)
	request := envelope{JSONRPC: "2.0", ID: jsontext.Value("1"), Method: "item/commandExecution/requestApproval", Params: params}
	if err := writeClientMessage(conn, request); err != nil {
		return clientServerOutcome{err: err}
	}
	if err := conn.SetReadDeadline(time.Now().Add(5 * time.Second)); err != nil {
		return clientServerOutcome{err: fmt.Errorf("set server deadline: %w", err)}
	}
	response, err := readEnvelope(bufio.NewReader(conn))
	if err != nil {
		return clientServerOutcome{err: fmt.Errorf("read client response: %w", err)}
	}
	return clientServerOutcome{response: response}
}

func writeClientMessage(w io.Writer, message envelope) error {
	body, err := json.Marshal(message)
	if err != nil {
		return fmt.Errorf("encode server message: %w", err)
	}
	if _, err := w.Write(append(body, '\n')); err != nil {
		return fmt.Errorf("write server message: %w", err)
	}
	return nil
}
