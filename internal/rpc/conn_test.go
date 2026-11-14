package rpc

import (
	"bufio"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"
)

func TestMissingJSONRPCAndUnknownMethod(t *testing.T) {
	left, right := net.Pipe()
	c := NewConn(t.Context(), left, nil)
	closeRPCPair(t, c, right)
	if _, err := right.Write([]byte(`{"id":1,"method":"missing","params":{}}` + "\n")); err != nil {
		t.Fatal(err)
	}
	line, err := bufio.NewReader(right).ReadBytes('\n')
	if err != nil {
		t.Fatal(err)
	}
	var got struct {
		Error wireError `json:"error"`
	}
	if err := json.Unmarshal(line, &got); err != nil {
		t.Fatal(err)
	}
	if got.Error.Code != -32601 {
		t.Fatalf("error=%+v", got.Error)
	}
}

func TestHandlerRPCErrorCodeReachesClient(t *testing.T) {
	left, right := net.Pipe()
	server := NewConn(t.Context(), left, func(context.Context, string, jsontext.Value) (jsontext.Value, error) {
		return nil, fmt.Errorf("start turn: %w", &RPCError{Code: -32600, Message: "thread t1 is running"})
	})
	client, err := Dial(right, nil)
	if err != nil {
		t.Fatal(err)
	}
	closeRPCPair(t, server, right)
	callErr := client.Call(t.Context(), "turn/start", nil, nil)
	coded, ok := errors.AsType[*RPCError](callErr)
	if !ok || coded.Code != -32600 || !strings.Contains(coded.Message, "thread t1 is running") {
		t.Fatalf("err=%v", callErr)
	}
}

func TestCallIDSendsIDAndRoutesResponse(t *testing.T) {
	left, right := net.Pipe()
	c := NewConn(t.Context(), left, nil)
	closeRPCPair(t, c, right)
	type result struct {
		Decision string `json:"decision"`
	}
	done := make(chan error, 1)
	var got result
	go func() {
		done <- c.CallID(t.Context(), "approval-7", "item/commandExecution/requestApproval", map[string]string{}, &got)
	}()
	line, err := bufio.NewReader(right).ReadBytes('\n')
	if err != nil {
		t.Fatal(err)
	}
	var request envelope
	if err := json.Unmarshal(line, &request); err != nil {
		t.Fatal(err)
	}
	if string(request.ID) != `"approval-7"` || request.Method != "item/commandExecution/requestApproval" {
		t.Fatalf("request=%s", line)
	}
	if _, err := right.Write([]byte(`{"id":"other","result":{"decision":"decline"}}` + "\n" + `{"id":"approval-7","result":{"decision":"accept"}}` + "\n")); err != nil {
		t.Fatal(err)
	}
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if got.Decision != "accept" {
		t.Fatalf("decision=%q", got.Decision)
	}
}

func TestDeltaQueueMerges(t *testing.T) {
	q := newOutQueue()
	for range 1024 {
		pushTestMessage(t, q, "item/agentMessage/delta", AgentMessageDeltaNotification{ThreadID: "thread", ItemID: "item", Delta: "x"})
	}
	pushTestMessage(t, q, "item/agentMessage/delta", AgentMessageDeltaNotification{ThreadID: "thread", ItemID: "item", Delta: "z"})
	if len(q.items) != 1024 {
		t.Fatalf("queued=%d, want 1024", len(q.items))
	}
	want := strings.Repeat("x", 1024) + "z"
	if got := popDeltaText(t, q, 1024); got != want {
		t.Fatalf("merged text length=%d, want %d", len(got), len(want))
	}
}

func TestMergedDeltaLinePreservesUnknownMembers(t *testing.T) {
	first, err := parseDelta(jsontext.Value(`{"threadId":"thread","itemId":"item","delta":"a\n\"","future":{"keep":true}}`))
	if err != nil {
		t.Fatal(err)
	}
	next, err := parseDelta(jsontext.Value(`{"itemId":"item","delta":"b\t"}`))
	if err != nil {
		t.Fatal(err)
	}
	items := []queued{{method: "item/agentMessage/delta", stream: &first}}
	if !mergeDelta(items, "item/agentMessage/delta", &next) {
		t.Fatal("delta did not merge")
	}
	line, err := items[0].line()
	if err != nil {
		t.Fatal(err)
	}
	var got struct {
		Params struct {
			Delta  string `json:"delta"`
			Future struct {
				Keep bool `json:"keep"`
			} `json:"future"`
		} `json:"params"`
	}
	if err := json.Unmarshal(line, &got); err != nil {
		t.Fatal(err)
	}
	if got.Params.Delta != "a\n\"b\t" || !got.Params.Future.Keep {
		t.Fatalf("merged params=%+v", got.Params)
	}
}

func TestDeltaQueuePreservesDistinctAndInvalidMessages(t *testing.T) {
	q := newOutQueue()
	params := jsontext.Value(`{"itemId":"a","delta":"x"}`)
	for range 1024 {
		pushTestMessage(t, q, "item/agentMessage/delta", params)
	}
	other := jsontext.Value(`{"itemId":"b","delta":"z"}`)
	pushTestMessage(t, q, "item/agentMessage/delta", other)
	pushTestMessage(t, q, "turn/started", jsontext.Value(`{"turnId":"t"}`))
	invalid := jsontext.Value(`{"itemId":`)
	if err := q.push(invalid, "item/agentMessage/delta", invalid, true, func() error { return nil }); err != nil {
		t.Fatal(err)
	}
	if len(q.items) != 1027 {
		t.Fatalf("queued=%d, want 1027", len(q.items))
	}
	if string(q.items[1024].stream.text) != "z" {
		t.Fatalf("different item merged: %q", q.items[1024].stream.text)
	}
	if q.items[1025].delta || q.items[1025].method != "turn/started" {
		t.Fatalf("non-delta changed: %+v", q.items[1025])
	}
	if q.items[1026].stream != nil || string(q.items[1026].b) != string(invalid) {
		t.Fatalf("invalid delta changed: %+v", q.items[1026])
	}
}

func TestSlowDeltaClientDoesNotBlockFastClient(t *testing.T) {
	slowServer, slowPeer := net.Pipe()
	slow := NewConn(t.Context(), slowServer, nil)
	closeRPCPair(t, slow, slowPeer)
	fastServer, fastPeer := net.Pipe()
	fast := NewConn(t.Context(), fastServer, nil)
	closeRPCPair(t, fast, fastPeer)

	const deltaCount = 10000
	sent := make(chan error, 1)
	go func() { sent <- sendAgentDeltas(slow, deltaCount) }()
	waitForDeltaProduction(t, sent)
	requireFastCompletion(t, fast, fastPeer)
	requireSlowDeltaText(t, slowPeer, deltaCount)
}

func TestNonDeltaQueueLimitClosesConnection(t *testing.T) {
	q := newOutQueue()
	closed := make(chan struct{})
	closeFn := func() error {
		q.mu.Lock()
		depth := len(q.items)
		q.mu.Unlock()
		if depth != 4096 {
			return fmt.Errorf("queue depth=%d, want 4096", depth)
		}
		close(closed)
		return nil
	}
	params := jsontext.Value(`{"turnId":"turn"}`)
	for range 4096 {
		if err := q.push([]byte(`{"method":"turn/started"}`), "turn/started", params, false, closeFn); err != nil {
			t.Fatal(err)
		}
	}
	if len(q.items) != 4096 {
		t.Fatalf("queue depth=%d", len(q.items))
	}
	err := q.push([]byte(`{"method":"turn/started"}`), "turn/started", params, false, closeFn)
	if !errors.Is(err, io.ErrShortBuffer) {
		t.Fatalf("push error=%v, want io.ErrShortBuffer", err)
	}
	select {
	case <-closed:
	case <-time.After(time.Second):
		t.Fatal("slow connection was not closed")
	}
}

func TestDeltaQueueLimitDistinctItems(t *testing.T) {
	q := newOutQueue()
	for i := range 4096 {
		item := queued{method: "item/agentMessage/delta", delta: true, stream: &queuedDelta{itemID: fmt.Sprintf("item-%d", i)}}
		if !q.append(item) {
			t.Fatalf("queue rejected item %d", i)
		}
	}
	closed := make(chan struct{})
	closeFn := func() error {
		close(closed)
		return nil
	}
	params := jsontext.Value(`{"itemId":"new","delta":"x"}`)
	err := q.push([]byte(`{"method":"item/agentMessage/delta"}`), "item/agentMessage/delta", params, true, closeFn)
	if !errors.Is(err, io.ErrShortBuffer) {
		t.Fatalf("push error=%v, want io.ErrShortBuffer", err)
	}
	select {
	case <-closed:
	case <-time.After(time.Second):
		t.Fatal("delta-only slow connection was not closed")
	}
}

func TestResponseEncodeFailureClosesConnection(t *testing.T) {
	left, right := net.Pipe()
	c := NewConn(t.Context(), left, func(context.Context, string, jsontext.Value) (jsontext.Value, error) {
		return jsontext.Value(`{"broken":`), nil
	})
	closeRPCPair(t, c, right)
	if _, err := right.Write([]byte(`{"id":1,"method":"broken"}` + "\n")); err != nil {
		t.Fatal(err)
	}
	select {
	case <-c.closed:
	case <-time.After(time.Second):
		t.Fatal("connection stayed open after response encoding failed")
	}
}

func TestPendingCallReturnsClosedPipe(t *testing.T) {
	left, right := net.Pipe()
	c := NewConn(t.Context(), left, nil)
	closeRPCPair(t, c, right)
	result := make(chan error, 1)
	go func() { result <- c.Call(t.Context(), "work", nil, nil) }()
	if _, err := bufio.NewReader(right).ReadBytes('\n'); err != nil {
		t.Fatal(err)
	}
	if err := c.Close(); err != nil {
		t.Fatal(err)
	}
	if err := <-result; !errors.Is(err, io.ErrClosedPipe) {
		t.Fatalf("call error=%v, want io.ErrClosedPipe", err)
	}
}

func TestCallCancellation(t *testing.T) {
	left, right := net.Pipe()
	c := NewConn(t.Context(), left, nil)
	closeRPCPair(t, c, right)
	ctx, cancel := context.WithTimeout(t.Context(), time.Millisecond)
	defer cancel()
	if err := c.Call(ctx, "x", nil, nil); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("call error=%v, want context deadline", err)
	}
}

func waitForDeltaProduction(t *testing.T, sent <-chan error) {
	t.Helper()
	select {
	case err := <-sent:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("slow client blocked delta production")
	}
}

func requireFastCompletion(t *testing.T, fast *Conn, peer net.Conn) {
	t.Helper()
	notification := TurnCompletedNotification{ThreadID: "thread", Turn: Turn{ID: "turn", Status: "completed"}}
	if err := fast.Notify("turn/completed", notification); err != nil {
		t.Fatal(err)
	}
	if err := peer.SetReadDeadline(time.Now().Add(time.Second)); err != nil {
		t.Fatal(err)
	}
	message, err := readEnvelope(bufio.NewReader(peer))
	if err != nil {
		t.Fatal(err)
	}
	if message.Method != "turn/completed" {
		t.Fatalf("fast method=%q", message.Method)
	}
}

func requireSlowDeltaText(t *testing.T, peer net.Conn, count int) {
	t.Helper()
	if err := peer.SetReadDeadline(time.Now().Add(5 * time.Second)); err != nil {
		t.Fatal(err)
	}
	text, err := readAgentDeltas(peer, count)
	if err != nil {
		t.Fatal(err)
	}
	if text != strings.Repeat("x", count) {
		t.Fatalf("slow client received %d bytes, want %d", len(text), count)
	}
}

func pushTestMessage(t *testing.T, q *outQueue, method string, params any) {
	t.Helper()
	rawParams, err := json.Marshal(params)
	if err != nil {
		t.Fatal(err)
	}
	body, err := json.Marshal(envelope{JSONRPC: "2.0", Method: method, Params: rawParams})
	if err != nil {
		t.Fatal(err)
	}
	if err := q.push(body, method, rawParams, true, func() error { return nil }); err != nil {
		t.Fatal(err)
	}
}

func popDeltaText(t *testing.T, q *outQueue, count int) string {
	t.Helper()
	var text strings.Builder
	for range count {
		item, ok := q.pop(nil)
		if !ok {
			t.Fatal("queue closed before all deltas arrived")
		}
		body, err := item.line()
		if err != nil {
			t.Fatal(err)
		}
		var env envelope
		if err := json.Unmarshal(body, &env); err != nil {
			t.Fatal(err)
		}
		var params AgentMessageDeltaNotification
		if err := json.Unmarshal(env.Params, &params); err != nil {
			t.Fatal(err)
		}
		text.WriteString(params.Delta)
	}
	return text.String()
}

func sendAgentDeltas(c *Conn, count int) error {
	for range count {
		params := AgentMessageDeltaNotification{ThreadID: "thread", ItemID: "item", Delta: "x"}
		if err := c.Notify("item/agentMessage/delta", params); err != nil {
			return fmt.Errorf("send delta: %w", err)
		}
	}
	return nil
}

func readAgentDeltas(r io.Reader, count int) (string, error) {
	reader := bufio.NewReader(r)
	var text strings.Builder
	text.Grow(count)
	for text.Len() < count {
		env, err := readEnvelope(reader)
		if err != nil {
			return "", fmt.Errorf("read delta: %w", err)
		}
		if env.Method != "item/agentMessage/delta" {
			return "", fmt.Errorf("read method %q", env.Method)
		}
		var params AgentMessageDeltaNotification
		if err := json.Unmarshal(env.Params, &params); err != nil {
			return "", fmt.Errorf("decode delta: %w", err)
		}
		text.WriteString(params.Delta)
	}
	return text.String(), nil
}

func readEnvelope(reader *bufio.Reader) (envelope, error) {
	line, err := reader.ReadBytes('\n')
	if err != nil {
		return envelope{}, fmt.Errorf("read line: %w", err)
	}
	var env envelope
	if err := json.Unmarshal(line, &env); err != nil {
		return envelope{}, fmt.Errorf("decode line: %w", err)
	}
	return env, nil
}

func closeRPCPair(t *testing.T, c *Conn, peer net.Conn) {
	t.Helper()
	t.Cleanup(func() {
		if err := c.Close(); err != nil {
			t.Errorf("close RPC connection: %v", err)
		}
		if err := peer.Close(); err != nil && !errors.Is(err, net.ErrClosed) {
			t.Errorf("close RPC peer: %v", err)
		}
	})
}

func readReply(t *testing.T, right net.Conn) []byte {
	t.Helper()
	reply := make(chan []byte, 1)
	go func() {
		line, err := bufio.NewReader(right).ReadBytes('\n')
		if err == nil {
			reply <- line
		}
	}()
	select {
	case line := <-reply:
		return line
	case <-time.After(5 * time.Second):
		t.Fatal("no reply for malformed envelope")
		return nil
	}
}

func TestWrongTypeRequestGetsInvalidRequest(t *testing.T) {
	left, right := net.Pipe()
	c := NewConn(t.Context(), left, nil)
	defer c.Close()
	defer right.Close()
	if _, err := right.Write([]byte(`{"id":3,"method":53}` + "\n")); err != nil {
		t.Fatal(err)
	}
	line := readReply(t, right)
	var got struct {
		ID    int `json:"id"`
		Error *struct {
			Code int `json:"code"`
		} `json:"error"`
	}
	if err := json.Unmarshal(line, &got); err != nil {
		t.Fatal(err)
	}
	if got.ID != 3 || got.Error == nil || got.Error.Code != -32600 {
		t.Fatalf("reply=%s", line)
	}
}

func TestInvalidJSONGetsParseError(t *testing.T) {
	left, right := net.Pipe()
	c := NewConn(t.Context(), left, nil)
	defer c.Close()
	defer right.Close()
	if _, err := right.Write([]byte(`{"id":3,"method":` + "\n")); err != nil {
		t.Fatal(err)
	}
	line := readReply(t, right)
	var got struct {
		ID    *int `json:"id"`
		Error *struct {
			Code int `json:"code"`
		} `json:"error"`
	}
	if err := json.Unmarshal(line, &got); err != nil {
		t.Fatal(err)
	}
	if got.Error == nil || got.Error.Code != -32700 {
		t.Fatalf("reply=%s", line)
	}
	if got.ID != nil {
		t.Fatalf("parse error id must be null, reply=%s", line)
	}
}

func notifyDelta(tb testing.TB, c *Conn, itemID, delta string) {
	tb.Helper()
	params := map[string]string{"threadId": "t", "itemId": itemID, "delta": delta}
	if err := c.Notify("item/agentMessage/delta", params); err != nil {
		tb.Fatal(err)
	}
}

func newPipedConn(tb testing.TB) (*Conn, func()) {
	tb.Helper()
	server, peer := net.Pipe()
	c := NewConn(tb.Context(), server, nil)
	return c, func() {
		_ = peer.Close()
		_ = c.Close()
	}
}

func BenchmarkDeltaMerge(b *testing.B) {
	c, done := newPipedConn(b)
	defer done()
	for range 1024 {
		notifyDelta(b, c, "same", "x")
	}
	b.ResetTimer()
	for b.Loop() {
		notifyDelta(b, c, "same", "y")
	}
}

func TestDeltaMergeNotifyLatency(t *testing.T) {
	const distinct = 1024
	const pushes = 2000
	c, done := newPipedConn(t)
	defer done()
	for i := range distinct {
		notifyDelta(t, c, "fill-"+strconv.Itoa(i), "x")
	}
	lat := make([]time.Duration, 0, pushes)
	for i := range pushes {
		start := time.Now()
		notifyDelta(t, c, "new-"+strconv.Itoa(i), "y")
		lat = append(lat, time.Since(start))
	}
	slices.Sort(lat)
	pct := func(p float64) time.Duration { return lat[int(p*float64(len(lat)-1))] }
	t.Logf("notify push latency p50=%v p95=%v p99=%v max=%v", pct(0.50), pct(0.95), pct(0.99), lat[len(lat)-1])
}
