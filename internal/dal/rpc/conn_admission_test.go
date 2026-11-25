package rpc

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"net"
	"sync/atomic"
	"testing"
)

func TestAdmissionLossSuppressesInFlightReply(t *testing.T) {
	admitted := atomic.Bool{}
	admitted.Store(true)
	serverCtx := WithDeviceAdmission(t.Context(), admitted.Load)
	left, right := net.Pipe()
	entered := make(chan struct{})
	release := make(chan struct{})
	server := NewConn(serverCtx, left, func(context.Context, string, jsontext.Value) (jsontext.Value, error) {
		close(entered)
		<-release
		return nil, errors.New("context canceled")
	})
	defer func() { _ = server.Close() }()
	client := Dial(t.Context(), right, nil)
	defer func() { _ = client.Close() }()
	callErr := make(chan error, 1)
	go func() { callErr <- client.Call(t.Context(), "hold", nil, nil) }()
	<-entered
	admitted.Store(false)
	close(release)
	if err := <-callErr; !errors.Is(err, io.ErrClosedPipe) {
		t.Fatalf("revoked call returned %v, want io.ErrClosedPipe", err)
	}
}
