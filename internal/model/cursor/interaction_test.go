package cursor_test

import (
	"context"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"google.golang.org/protobuf/encoding/protowire"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/cursor"
	"github.com/gosuda/dalgona/internal/model/cursor/wire"
)

func protoField(t *testing.T, b []byte, want protowire.Number) []byte {
	t.Helper()
	for len(b) > 0 {
		number, kind, size := protowire.ConsumeTag(b)
		if size < 0 {
			abortf(t, "bad tag in %x", b)
		}
		b = b[size:]
		valueSize := protowire.ConsumeFieldValue(number, kind, b)
		if valueSize < 0 {
			abortf(t, "bad field %d in %x", number, b)
		}
		if number == want && kind == protowire.BytesType {
			value, _ := protowire.ConsumeBytes(b)
			return value
		}
		b = b[valueSize:]
	}
	abortf(t, "field %d missing", want)
	return nil
}

func sendQuery(c *fakeConn, id uint32, query int32) {
	view := protowire.AppendTag(nil, 1, protowire.VarintType)
	view = protowire.AppendVarint(view, uint64(id))
	view = protowire.AppendTag(view, protowire.Number(query), protowire.BytesType)
	view = protowire.AppendBytes(view, nil)
	c.send(&wire.AgentServerMessage{MessageNumber: 7, MessageView: view})
}

func (c *fakeConn) interactionResponse(id uint32, result protowire.Number) []byte {
	c.t.Helper()
	message := c.next()
	if message.MessageNumber != 6 {
		abortf(c.t, "client message = %#v, want interaction response", message)
	}
	gotID, size := protowire.ConsumeVarint(message.MessageView[1:])
	if message.MessageView[0] != 0x08 || size < 0 || gotID != uint64(id) {
		abortf(c.t, "interaction response %x, want id %d", message.MessageView, id)
	}
	return protoField(c.t, message.MessageView, result)
}

func TestInteractionQueriesAreRefused(t *testing.T) {
	const reason = "not supported by this client"
	_, server := newServer(t, func(c *fakeConn) {
		for index, query := range []int32{2, 4, 5, 6, 9} {
			id := uint32(index + 10)
			sendQuery(c, id, query)
			rejected := protoField(t, c.interactionResponse(id, protowire.Number(query)), 2)
			if text := string(protoField(t, rejected, 1)); !strings.Contains(text, reason) {
				t.Errorf("query %d rejection = %q", query, text)
			}
		}
		sendQuery(c, 30, 3)
		asked := protoField(t, protoField(t, protoField(t, c.interactionResponse(30, 3), 1), 3), 1)
		sendQuery(c, 31, 7)
		planned := protoField(t, protoField(t, protoField(t, c.interactionResponse(31, 7), 1), 2), 1)
		if !strings.Contains(string(asked), reason) || !strings.Contains(string(planned), reason) {
			t.Errorf("ask rejection = %q, plan error = %q", asked, planned)
		}
		c.endTurn()
	})
	stream, err := newProvider(server, time.Second).Stream(t.Context(), userRequest("hi"))
	if err != nil {
		t.Fatal(err)
	}
	if out := drain(t, stream); out.failure != nil || out.stop != "stop" {
		t.Fatalf("result = %+v", out)
	}
}

func (c *fakeConn) expectClosed() {
	c.t.Helper()
	timer := time.NewTimer(waitLimit)
	defer timer.Stop()
	for {
		select {
		case message, ok := <-c.inbound:
			if !ok {
				return
			}
			if !isHeartbeat(message) {
				c.t.Errorf("client message = %#v, want stream close", message)
			}
		case <-timer.C:
			c.t.Error("client kept the stream open")
			return
		}
	}
}

func TestSetupVMQueryFailsTurn(t *testing.T) {
	_, server := newServer(t, func(c *fakeConn) {
		sendQuery(c, 40, 8)
		c.expectClosed()
	})
	stream, err := newProvider(server, time.Second).Stream(t.Context(), userRequest("hi"))
	if err != nil {
		t.Fatal(err)
	}
	out := drain(t, stream)
	if out.failure == nil || !strings.Contains(out.failure.Err.Error(), "interaction query 8") {
		t.Fatalf("result = %+v", out)
	}
}

func execAbort(id uint32) *wire.AgentServerMessage {
	abort := protowire.AppendTag(nil, 1, protowire.VarintType)
	abort = protowire.AppendVarint(abort, uint64(id))
	control := protowire.AppendTag(nil, 1, protowire.BytesType)
	control = protowire.AppendBytes(control, abort)
	return &wire.AgentServerMessage{MessageNumber: 5, MessageView: control}
}

func TestExecAbortCancelsInBandCall(t *testing.T) {
	var cancelled atomic.Bool
	blocking := func(ctx context.Context, call model.ToolCall) (model.ToolResult, error) {
		select {
		case <-ctx.Done():
			cancelled.Store(true)
			return model.ToolResult{}, ctx.Err()
		case <-time.After(waitLimit):
			return model.ToolResult{ID: call.ID, Name: call.Name, Output: []byte(`"done"`)}, nil
		}
	}
	_, server := newServer(t, func(c *fakeConn) {
		c.sendExec(shellExec(9))
		c.send(&wire.AgentServerMessage{Interaction: &wire.InteractionUpdate{VariantNumber: 13, VariantView: []byte{}}})
		c.send(&wire.AgentServerMessage{Interaction: &wire.InteractionUpdate{Text: &wire.TextDelta{TextView: []byte("queued")}}})
		c.send(execAbort(9))
		c.expectStreamClose(9)
		c.endTurn()
	})
	start := time.Now()
	stream, err := newProvider(server, time.Second).StreamWith(t.Context(), userRequest("run it"), cursor.StreamOptions{InBand: blocking})
	if err != nil {
		t.Fatal(err)
	}
	out := drain(t, stream)
	if out.failure != nil || out.stop != "stop" || out.text != "queued" || !cancelled.Load() || time.Since(start) >= waitLimit {
		t.Fatalf("result = %+v, cancelled = %v, elapsed = %v", out, cancelled.Load(), time.Since(start))
	}
	checkResolvedExec(t, out.calls)
}

func TestFrameBacklogDuringCallFailsTurn(t *testing.T) {
	var cancelled atomic.Bool
	blocking := func(ctx context.Context, call model.ToolCall) (model.ToolResult, error) {
		select {
		case <-ctx.Done():
			cancelled.Store(true)
			return model.ToolResult{}, ctx.Err()
		case <-time.After(waitLimit):
			return model.ToolResult{ID: call.ID, Name: call.Name, Output: []byte(`"done"`)}, nil
		}
	}
	_, server := newServer(t, func(c *fakeConn) {
		c.sendExec(shellExec(9))
		for range 64 {
			c.send(&wire.AgentServerMessage{Interaction: &wire.InteractionUpdate{Text: &wire.TextDelta{TextView: []byte("x")}}})
		}
		c.expectStreamClose(9)
		c.expectClosed()
	})
	start := time.Now()
	stream, err := newProvider(server, time.Second).StreamWith(t.Context(), userRequest("run it"), cursor.StreamOptions{InBand: blocking})
	if err != nil {
		t.Fatal(err)
	}
	out := drain(t, stream)
	if out.failure == nil || !strings.Contains(out.failure.Err.Error(), "64 server frames") || !cancelled.Load() || time.Since(start) >= waitLimit {
		t.Fatalf("result = %+v, cancelled = %v, elapsed = %v", out, cancelled.Load(), time.Since(start))
	}
}

func TestExecAbortForQueuedCallSkipsIt(t *testing.T) {
	counter := &inBandCounter{delay: 200 * time.Millisecond}
	_, server := newServer(t, func(c *fakeConn) {
		c.sendExec(shellExec(9))
		c.sendExec(shellExec(10))
		c.send(execAbort(10))
		checkShellReply(t, c.execReply())
		c.expectStreamClose(9)
		c.expectStreamClose(10)
		c.endTurn()
	})
	stream, err := newProvider(server, time.Second).StreamWith(t.Context(), userRequest("run it"), cursor.StreamOptions{InBand: counter.run})
	if err != nil {
		t.Fatal(err)
	}
	if out := drain(t, stream); out.failure != nil || out.stop != "stop" || counter.calls.Load() != 1 {
		t.Fatalf("result = %+v, calls = %d", out, counter.calls.Load())
	}
}

func TestShellKillDeadlineIsHardTimeout(t *testing.T) {
	counter := &inBandCounter{}
	_, server := newServer(t, func(c *fakeConn) {
		hard := shellExec(9)
		hard.Shell.Timeout, hard.Shell.HardTimeout = 5000, 90000
		c.sendExec(hard)
		c.execReply()
		c.expectStreamClose(9)
		waitOnly := shellExec(10)
		waitOnly.Shell.Timeout = 5000
		c.sendExec(waitOnly)
		c.execReply()
		c.expectStreamClose(10)
		c.endTurn()
	})
	stream, err := newProvider(server, time.Second).StreamWith(t.Context(), userRequest("run it"), cursor.StreamOptions{InBand: counter.run})
	if err != nil {
		t.Fatal(err)
	}
	if out := drain(t, stream); out.failure != nil {
		t.Fatalf("result = %+v", out)
	}
	counter.mu.Lock()
	defer counter.mu.Unlock()
	want := []string{`{"command":"echo hi","timeout_ms":90000}`, `{"command":"echo hi"}`}
	if len(counter.seen) != len(want) {
		t.Fatalf("calls = %+v", counter.seen)
	}
	for index, call := range counter.seen {
		if string(call.Args) != want[index] {
			t.Errorf("call %d args = %s, want %s", index, call.Args, want[index])
		}
	}
}
