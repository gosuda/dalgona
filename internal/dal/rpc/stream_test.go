package rpc

import (
	"bufio"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"math/rand/v2"
	"net"
	"strings"
	"sync"
	"testing"
	"time"
	"unicode/utf8"
)

type streamRead struct {
	events []Event
	lines  []int
	err    error
}

func readStream(reader *bufio.Reader, streamID, stopMethod string) streamRead {
	var out streamRead
	for {
		line, err := reader.ReadBytes('\n')
		if err != nil {
			out.err = err
			return out
		}
		var env envelope
		if decodeErr := json.Unmarshal(line, &env); decodeErr != nil {
			out.err = decodeErr
			return out
		}
		event, err := ParseEvent(streamID, env.Params)
		if err != nil {
			out.err = err
			return out
		}
		out.events = append(out.events, event)
		out.lines = append(out.lines, len(line)-1)
		if event.Method == stopMethod {
			return out
		}
	}
}

type deltaSource struct {
	mu   sync.Mutex
	conn *Conn
	text []byte
	cuts []sourceCut
}

type sourceCut struct {
	ordinal uint64
	prefix  int
}

func (s *deltaSource) publish(t *testing.T, delta string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	params, err := json.Marshal(map[string]string{"threadId": "t1", "turnId": "u1", "itemId": "i1", "delta": delta})
	if err != nil {
		t.Error(err)
		return
	}
	if err := s.conn.Notify("item/agentMessage/delta", jsontext.Value(params)); err != nil {
		t.Error(err)
	}
	s.text = append(s.text, delta...)
}

func (s *deltaSource) cut(t *testing.T) {
	s.mu.Lock()
	defer s.mu.Unlock()
	cut, ok := s.conn.Cut("t1")
	if !ok {
		t.Error("cut on an opened stream reported no stream")
		return
	}
	s.cuts = append(s.cuts, sourceCut{ordinal: cut.Ordinal, prefix: len(s.text)})
}

func randomDelta(r *rand.Rand) string {
	alphabet := []string{"a", "é", "한", "😀", "\n", "\"", "\x01"}
	size := 1 + r.IntN(40)
	if r.IntN(25) == 0 {
		size = 30000 + r.IntN(20000)
	}
	var b strings.Builder
	for range size {
		b.WriteString(alphabet[r.IntN(len(alphabet))])
	}
	return b.String()
}

func TestOrderedStreamCutsNeverSplitCoalescedDeltas(t *testing.T) {
	left, right := net.Pipe()
	server := NewConn(t.Context(), left, nil)
	closeRPCPair(t, server, right)
	server.OpenStream("s1", nil)
	source := &deltaSource{conn: server}
	r := rand.New(rand.NewPCG(1, 2))
	release := make(chan struct{})
	reader := bufio.NewReader(right)
	done := make(chan streamRead, 1)
	go func() {
		<-release
		done <- readStream(reader, "s1", "thread/status/changed")
	}()
	for i := range 600 {
		source.publish(t, randomDelta(r))
		if i%37 == 0 {
			source.cut(t)
		}
		if i == 120 {
			close(release)
		}
	}
	source.cut(t)
	if err := server.Notify("thread/status/changed", map[string]any{"threadId": "t1", "status": map[string]string{"type": "idle"}}); err != nil {
		t.Fatal(err)
	}
	var got streamRead
	select {
	case got = <-done:
	case <-time.After(20 * time.Second):
		t.Fatal("stream reader timed out")
	}
	if got.err != nil {
		t.Fatal(got.err)
	}
	assertStreamReconciles(t, source, got)
}

type reconcileState struct {
	next   uint64
	text   []byte
	merged bool
}

func assertStreamReconciles(t *testing.T, source *deltaSource, got streamRead) {
	t.Helper()
	state := reconcileState{next: 1}
	for i, event := range got.events {
		state.checkEvent(t, source, got, i, event)
	}
	if !bytes.Equal(state.text, source.text) {
		t.Fatalf("reconciled %d bytes, source published %d bytes", len(state.text), len(source.text))
	}
	if !state.merged {
		t.Fatal("no envelope coalesced consecutive deltas; the test did not exercise coalescing")
	}
}

func (s *reconcileState) checkEvent(t *testing.T, source *deltaSource, got streamRead, i int, event Event) {
	t.Helper()
	if event.First != s.next {
		t.Fatalf("event %d starts at ordinal %d, want %d", i, event.First, s.next)
	}
	s.next = event.Last + 1
	s.merged = s.merged || event.Last > event.First
	if got.lines[i] > EventEnvelopeLimit {
		t.Fatalf("event %d envelope is %d bytes, over %d", i, got.lines[i], EventEnvelopeLimit)
	}
	piece := deltaOf(t, event)
	if len(piece) > TextDeltaLimit {
		t.Fatalf("event %d carries %d delta bytes, over %d", i, len(piece), TextDeltaLimit)
	}
	for _, cut := range source.cuts {
		checkCut(t, event, cut, i, len(s.text)+len(piece))
	}
	s.text = append(s.text, piece...)
}

func checkCut(t *testing.T, event Event, cut sourceCut, i, through int) {
	t.Helper()
	if _, err := event.AfterCut(cut.ordinal); err != nil {
		t.Fatalf("event %d: %v", i, err)
	}
	if event.Last == cut.ordinal && through != cut.prefix {
		t.Fatalf("text through cut %d is %d bytes, source had %d", cut.ordinal, through, cut.prefix)
	}
}

func deltaOf(t *testing.T, event Event) []byte {
	t.Helper()
	if event.Method != "item/agentMessage/delta" {
		return nil
	}
	var params struct {
		Delta string `json:"delta"`
	}
	if err := json.Unmarshal(event.Params, &params); err != nil {
		t.Fatal(err)
	}
	return []byte(params.Delta)
}

func TestSplitTextKeepsUTF8AndBudgets(t *testing.T) {
	text := []byte(strings.Repeat("a한😀\n", 40000))
	chunks := splitText(text, TextDeltaLimit, 100000)
	joined := checkTextChunks(t, chunks, TextDeltaLimit, 100000)
	if !bytes.Equal(joined, text) || len(chunks) < 2 {
		t.Fatalf("split into %d chunks that rejoin to %d of %d bytes", len(chunks), len(joined), len(text))
	}
	checkControlSplit(t)
}

func checkTextChunks(t *testing.T, chunks []textChunk, rawLimit, escapedLimit int) []byte {
	t.Helper()
	var joined []byte
	for i, chunk := range chunks {
		checkTextChunk(t, chunk, i, rawLimit, escapedLimit)
		joined = append(joined, chunk.text...)
	}
	return joined
}

func checkTextChunk(t *testing.T, chunk textChunk, i, rawLimit, escapedLimit int) {
	t.Helper()
	if !utf8.Valid(chunk.text) {
		t.Fatalf("chunk %d splits a UTF-8 sequence", i)
	}
	if len(chunk.text) > rawLimit {
		t.Fatalf("chunk %d has %d bytes", i, len(chunk.text))
	}
	quoted, err := jsontext.AppendQuote(nil, chunk.text)
	if err != nil {
		t.Fatal(err)
	}
	if len(quoted)-2 != chunk.escaped || chunk.escaped > escapedLimit {
		t.Fatalf("chunk %d escapes to %d bytes, accounted %d", i, len(quoted)-2, chunk.escaped)
	}
}

func checkControlSplit(t *testing.T) {
	t.Helper()
	controls := splitText([]byte(strings.Repeat("\x01", 30000)), TextDeltaLimit, 60000)
	if len(controls) != 3 || controls[0].escaped != 60000 {
		t.Fatalf("control text split into %d chunks with first escaped %d", len(controls), controls[0].escaped)
	}
}

func TestOversizedEventMovesParamsToContentReference(t *testing.T) {
	left, right := net.Pipe()
	server := NewConn(t.Context(), left, nil)
	closeRPCPair(t, server, right)
	var spilled jsontext.Value
	var spillOrdinal uint64
	server.OpenStream("s1", func(threadID string, ordinal uint64, params jsontext.Value) (ContentReference, error) {
		spilled, spillOrdinal = bytes.Clone(params), ordinal
		sum := sha256.Sum256(params)
		return ContentReference{Source: ContentSource{Kind: "live", StreamID: "s1"}, ContentID: "c1", Field: "params", Encoding: "json", Total: FormatDecimal(uint64(len(params))), Digest: hex.EncodeToString(sum[:])}, nil
	})
	big := strings.Repeat("x", EventEnvelopeLimit)
	go func() {
		if err := server.Notify("warning", map[string]string{"threadId": "t1", "message": big}); err != nil {
			t.Error(err)
		}
	}()
	got := readStream(bufio.NewReader(right), "s1", "warning")
	if got.err != nil {
		t.Fatal(got.err)
	}
	event := got.events[0]
	if event.ParamsContent == nil || len(event.Params) != 0 || got.lines[0] > EventEnvelopeLimit {
		t.Fatalf("oversized event = %+v (%d bytes)", event, got.lines[0])
	}
	if !strings.Contains(string(spilled), big) {
		t.Fatal("spill did not receive the original params")
	}
	if spillOrdinal != event.First {
		t.Fatalf("spill ordinal = %d, want the event ordinal %d", spillOrdinal, event.First)
	}
}

func TestPublishReturnsTheLastAssignedOrdinal(t *testing.T) {
	left, right := net.Pipe()
	server := NewConn(t.Context(), left, nil)
	closeRPCPair(t, server, right)
	server.OpenStream("s1", nil)
	reader := bufio.NewReader(right)
	ordinal, err := server.Publish("t1", "warning", jsontext.Value(`{"threadId":"t1","message":"first"}`))
	first := readStream(reader, "s1", "warning")
	if err != nil || first.err != nil || ordinal != first.events[0].Last {
		t.Fatalf("Publish = %d, %v; wire event %+v (%v)", ordinal, err, first.events, first.err)
	}
	delta := strings.Repeat("d", TextDeltaLimit+1)
	ordinal, err = server.Publish("t1", "item/agentMessage/delta", jsontext.Value(`{"threadId":"t1","turnId":"u","itemId":"i","delta":"`+delta+`"}`))
	if err != nil {
		t.Fatal(err)
	}
	var last uint64
	for range 2 {
		read := readStream(reader, "s1", "item/agentMessage/delta")
		if read.err != nil {
			t.Fatal(read.err)
		}
		last = read.events[len(read.events)-1].Last
	}
	if ordinal != 3 || ordinal != last {
		t.Fatalf("delta Publish = %d, want 3, the last wire ordinal %d of two split deltas after ordinal 1", ordinal, last)
	}
}

func TestNativeQueueClosesAtByteBound(t *testing.T) {
	left, right := net.Pipe()
	server := NewConn(t.Context(), left, nil)
	closeRPCPair(t, server, right)
	server.OpenStream("s1", nil)
	payload := strings.Repeat("y", 200<<10)
	accepted := 0
	for ; accepted < 4096; accepted++ {
		if err := server.Notify("warning", map[string]string{"threadId": "t1", "message": payload}); err != nil {
			if !errors.Is(err, io.ErrShortBuffer) {
				t.Fatalf("notify error = %v", err)
			}
			break
		}
	}
	if limit := NativeQueueBytes/len(payload) + 2; accepted > limit {
		t.Fatalf("queue accepted %d events of %d bytes, byte bound allows at most %d", accepted, len(payload), limit)
	}
	select {
	case <-server.Done():
	case <-time.After(5 * time.Second):
		t.Fatal("full native queue did not close the connection")
	}
}

func TestWrappedServerRequestKeepsDetailsUntilResolved(t *testing.T) {
	left, right := net.Pipe()
	server := NewConn(t.Context(), left, nil)
	closeRPCPair(t, server, right)
	server.OpenStream("s1", nil)
	raw := marshalApprovalCommand(t)
	answered := make(chan error, 1)
	go func() {
		answered <- server.CallID(t.Context(), "req-1", "item/commandExecution/requestApproval", jsontext.Value(raw), nil)
	}()
	request := readRequestEnvelope(t, right)
	checkRequestHeader(t, request, raw)
	checkPendingDetail(t, server, raw)
	answerRequest(t, right)
	if err := <-answered; err != nil {
		t.Fatal(err)
	}
	if _, ok := server.PendingRequest("req-1"); ok {
		t.Fatal("resolved request still exposes its details")
	}
}

func marshalApprovalCommand(t *testing.T) []byte {
	t.Helper()
	params := map[string]string{"threadId": "t1", "itemId": "call", "command": strings.Repeat("rm -rf x\n", 1000)}
	raw, err := json.Marshal(params)
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

func readRequestEnvelope(t *testing.T, right net.Conn) envelope {
	t.Helper()
	request, err := readEnvelope(bufio.NewReader(right))
	if err != nil {
		t.Fatal(err)
	}
	return request
}

func checkRequestHeader(t *testing.T, request envelope, raw []byte) {
	t.Helper()
	var header RequestParams
	if err := json.Unmarshal(request.Params, &header); err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(raw)
	if request.Method != MethodRequest || string(request.ID) != `"req-1"` || header.Method != "item/commandExecution/requestApproval" ||
		header.Details.Digest != hex.EncodeToString(sum[:]) || header.Details.Total != fmt.Sprint(len(raw)) || header.DalgonaEvent.Ordinal != "1" || len(request.Params) > RequestHeaderLimit {
		t.Fatalf("request = %s %s %s", request.Method, request.ID, request.Params)
	}
}

func checkPendingDetail(t *testing.T, server *Conn, raw []byte) {
	t.Helper()
	detail, ok := server.PendingRequest("req-1")
	if !ok || !bytes.Equal(detail.Params, raw) {
		t.Fatalf("pending detail = %+v %v", detail, ok)
	}
}

func answerRequest(t *testing.T, right net.Conn) {
	t.Helper()
	if _, err := fmt.Fprintf(right, "{\"id\":\"req-1\",\"result\":{\"decision\":\"accept\"}}\n"); err != nil {
		t.Fatal(err)
	}
}

func TestCancelRequestCancelsOnlyTheMatchingRunningHandler(t *testing.T) {
	left, right := net.Pipe()
	causes := make(chan error, 2)
	started := make(chan string, 2)
	server := NewConn(t.Context(), left, cancellableHandler(started, causes))
	server.OpenStream("s1", nil)
	closeRPCPair(t, server, right)
	reader := bufio.NewReader(right)
	writeLines(t, right, `{"jsonrpc":"2.0","id":7,"method":"slow/a"}`, `{"jsonrpc":"2.0","id":"7","method":"slow/b"}`)
	<-started
	<-started
	writeLines(t, right, `{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":99}}`, `{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":7}}`)
	checkCancelledReply(t, reader, causes)
	checkFinishedReply(t, reader)
}

func cancellableHandler(started chan<- string, causes chan<- error) func(context.Context, string, jsontext.Value) (jsontext.Value, error) {
	return func(ctx context.Context, method string, _ jsontext.Value) (jsontext.Value, error) {
		scoped, release := RequestScope(ctx)
		defer release()
		started <- method
		select {
		case <-scoped.Done():
			causes <- context.Cause(scoped)
			return nil, NativeKind(ErrorCancelled)
		case <-time.After(2 * time.Second):
			causes <- nil
			return jsontext.Value(`"finished"`), nil
		}
	}
}

func checkCancelledReply(t *testing.T, reader *bufio.Reader, causes <-chan error) {
	t.Helper()
	first, err := readEnvelope(reader)
	if err != nil {
		t.Fatal(err)
	}
	if string(first.ID) != "7" || first.Error == nil || first.Error.Code != -32019 || first.Error.Message != "cancelled" {
		t.Fatalf("cancelled reply = %s %+v", first.ID, first.Error)
	}
	if cause := <-causes; !errors.Is(cause, ErrRequestCancelled) {
		t.Fatalf("handler cause = %v", cause)
	}
}

func checkFinishedReply(t *testing.T, reader *bufio.Reader) {
	t.Helper()
	second, err := readEnvelope(reader)
	if err != nil {
		t.Fatal(err)
	}
	if string(second.ID) != `"7"` || second.Error != nil || string(second.Result) != `"finished"` {
		t.Fatalf("string id 7 reply = %s %+v %s", second.ID, second.Error, second.Result)
	}
}

func writeLines(t *testing.T, w io.Writer, lines ...string) {
	t.Helper()
	for _, line := range lines {
		if _, err := io.WriteString(w, line+"\n"); err != nil {
			t.Fatal(err)
		}
	}
}

func TestCallOperationReportsUnknownOutcomeAfterDispatch(t *testing.T) {
	left, right := net.Pipe()
	client := Dial(t.Context(), left, &orderHandler{})
	t.Cleanup(func() { _ = client.Close() })
	id := NewOperationID()
	result := make(chan error, 1)
	go func() {
		result <- client.CallOperation(t.Context(), Operation{Method: "turn/start", OperationID: id, Endpoint: "local", Generation: 3}, map[string]string{"threadId": "t1"}, nil)
	}()
	params := readOperationParams(t, right)
	if params[OperationMember] != id || params["threadId"] != "t1" {
		t.Fatalf("operation params = %v", params)
	}
	if err := right.Close(); err != nil {
		t.Fatal(err)
	}
	checkUnknownOutcome(t, <-result, id)
}

func readOperationParams(t *testing.T, right net.Conn) map[string]string {
	t.Helper()
	request, err := readEnvelope(bufio.NewReader(right))
	if err != nil {
		t.Fatal(err)
	}
	var params map[string]string
	if err := json.Unmarshal(request.Params, &params); err != nil {
		t.Fatal(err)
	}
	return params
}

func checkUnknownOutcome(t *testing.T, err error, id string) {
	t.Helper()
	unknown, ok := errors.AsType[*UnknownOutcomeError](err)
	if !ok || unknown.Method != "turn/start" || unknown.OperationID != id || unknown.Endpoint != "local" || unknown.Generation != 3 {
		t.Fatalf("error = %+v", unknown)
	}
}

func TestOperationTimeRejectsNonVersion7(t *testing.T) {
	now := time.Now()
	at, err := OperationTime(NewOperationID())
	if err != nil || at.Sub(now).Abs() > time.Second {
		t.Fatalf("operation time = %v, %v", at, err)
	}
	for _, id := range []string{"f81d4fae-7dec-11d0-a765-00a0c91e6bf6", "0190A0B0-0000-7000-8000-000000000000", "not-a-uuid"} {
		if _, err := OperationTime(id); err == nil {
			t.Fatalf("operation id %q accepted", id)
		}
	}
}
