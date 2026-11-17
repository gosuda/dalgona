package rpc

import (
	"bufio"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"net"
	"sync/atomic"
	"testing"
	"time"
)

type appliedProbe struct{ applied *atomic.Bool }

func (p *appliedProbe) UnmarshalJSON([]byte) error {
	p.applied.Store(true)
	return nil
}

type orderHandler struct {
	flags    []atomic.Bool
	observed chan bool
}

func (h *orderHandler) Notification(method string, params jsontext.Value) {
	var note struct {
		N int `json:"n"`
	}
	if method != "test/after" || json.Unmarshal(params, &note) != nil {
		return
	}
	h.observed <- h.flags[note.N].Load()
}

func (h *orderHandler) Request(context.Context, string, jsontext.Value) (any, error) {
	return nil, fmt.Errorf("unexpected server request")
}

func TestResponseAppliesBeforeFollowingNotification(t *testing.T) {
	const iterations = 300
	handler := &orderHandler{flags: make([]atomic.Bool, iterations), observed: make(chan bool, iterations)}
	left, right := net.Pipe()
	client := Dial(t.Context(), left, handler)
	closeRPCPair(t, client, right)
	reader := bufio.NewReader(right)
	timer := time.NewTimer(5 * time.Second)
	defer timer.Stop()
	for i := range iterations {
		callDone := make(chan error, 1)
		go func() {
			callDone <- client.Call(t.Context(), "probe", nil, &appliedProbe{applied: &handler.flags[i]})
		}()
		request, err := readEnvelope(reader)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := fmt.Fprintf(right, "{\"id\":%s,\"result\":\"ok\"}\n{\"method\":\"test/after\",\"params\":{\"n\":%d}}\n", request.ID, i); err != nil {
			t.Fatal(err)
		}
		handler.awaitResponseApplication(t, callDone, timer, i)
	}
}

func (h *orderHandler) awaitResponseApplication(t *testing.T, callDone <-chan error, timer *time.Timer, iteration int) {
	t.Helper()
	timer.Reset(5 * time.Second)
	select {
	case applied := <-h.observed:
		if !applied {
			t.Fatalf("iteration %d: notification handler ran before the response reached its call", iteration)
		}
	case <-timer.C:
		t.Fatalf("iteration %d: timed out waiting for notification handler", iteration)
	}
	timer.Reset(5 * time.Second)
	select {
	case err := <-callDone:
		if err != nil {
			t.Fatal(err)
		}
	case <-timer.C:
		t.Fatalf("iteration %d: timed out waiting for call to finish", iteration)
	}
}
