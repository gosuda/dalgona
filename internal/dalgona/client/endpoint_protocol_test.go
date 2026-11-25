package client

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"io"
	"net"
	"sync/atomic"
	"testing"
	"testing/synctest"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func TestTUIProfileUsesCodexHandshakeOnEveryGeneration(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		rec := newRecorder()
		var initialized atomic.Int32
		spec := EndpointSpec{ID: "plain", Dial: func(ctx context.Context) (io.ReadWriteCloser, error) {
			near, far := net.Pipe()
			_ = rpc.NewConn(context.WithoutCancel(ctx), far, codexOnlyHandshake(t, &initialized))
			return near, nil
		}}
		set, err := NewEndpointSet(nil, []EndpointSpec{spec}, rec, ProtocolTUI)
		if err != nil {
			t.Fatal(err)
		}
		defer func() { _ = set.Close() }()
		for _, tc := range []tuiGenerationCase{
			{generation: 1, wantInitialized: 1},
			{generation: 2, wantInitialized: 2},
		} {
			tc.set = set
			tc.rec = rec
			tc.id = spec.ID
			tc.initialized = &initialized
			checkTUIProfileGeneration(t, tc)
		}
	})
}

type tuiGenerationCase struct {
	set             *EndpointSet
	rec             *recorder
	id              EndpointID
	generation      Generation
	wantInitialized int32
	initialized     *atomic.Int32
}

func checkTUIProfileGeneration(t *testing.T, c tuiGenerationCase) {
	t.Helper()
	if err := c.set.Connect(c.id); err != nil {
		t.Fatal(err)
	}
	synctest.Wait()
	assertTUIProfileHandshake(t, c.rec.last(c.id), c.generation)
	if c.initialized.Load() != c.wantInitialized {
		t.Fatal("initialized notification missing or sent more than once")
	}
	if err := c.set.Disconnect(c.id); err != nil {
		t.Fatal(err)
	}
}

func assertTUIProfileHandshake(t *testing.T, change PhaseChange, generation Generation) {
	t.Helper()
	if change.Phase != PhaseConnected || change.Initialized.UserAgent != "dal/test" || change.Link.Generation != generation {
		t.Fatalf("expected initialized Codex-only generation: %+v", change)
	}
}

func codexOnlyHandshake(t *testing.T, initialized *atomic.Int32) rpc.Handler {
	return func(_ context.Context, method string, raw jsontext.Value) (jsontext.Value, error) {
		switch method {
		case "initialize":
			var params rpc.InitializeParams
			if err := json.Unmarshal(raw, &params); err != nil || params.Capabilities != nil {
				t.Errorf("TUI requested unexpected capabilities: %v", err)
			}
			return jsontext.Value(`{"userAgent":"dal/test","codexHome":"/home/dal","platformFamily":"unix","platformOs":"linux"}`), nil
		case "initialized":
			initialized.Add(1)
			return nil, nil
		default:
			t.Errorf("TUI sent a non-Codex handshake call: %q", method)
			return nil, &rpc.RPCError{Code: -32601, Message: "Method not found"}
		}
	}
}
