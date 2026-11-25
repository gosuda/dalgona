package rpcserver

import (
	"bufio"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"io"
	"log/slog"
	"math/rand/v2"
	"net"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/config"
)

type nativeClient struct {
	t        *testing.T
	conn     net.Conn
	writeMu  sync.Mutex
	mu       sync.Mutex
	next     int
	waiting  map[string]chan message
	events   chan rpc.Event
	requests chan message
	plain    chan message
	streamID string
	ordinal  uint64
}

func newNativeClient(t *testing.T, conn net.Conn) *nativeClient {
	c := &nativeClient{
		t: t, conn: conn, waiting: map[string]chan message{},
		events: make(chan rpc.Event, 100000), requests: make(chan message, 64), plain: make(chan message, 100000),
	}
	go c.read()
	return c
}

func (r *rig) connectNative(opts ServeOptions) *nativeClient {
	r.t.Helper()
	left, right := net.Pipe()
	c := newNativeClient(r.t, right)
	served := make(chan error, 1)
	go func() { served <- ServeWith(r.t.Context(), left, r.host, opts) }()
	r.t.Cleanup(func() {
		_ = c.conn.Close()
		select {
		case serveErr := <-served:
			_ = serveErr
		case <-time.After(waitLimit):
			r.t.Error("Serve did not return after the native client closed")
		}
	})
	c.negotiate()
	return c
}

func (c *nativeClient) negotiate() rpc.InitializeResult {
	c.t.Helper()
	var init rpc.InitializeResult
	c.ok("initialize", nativeInitialize(rpc.NativeVersion, rpc.NativeDelivery), &init)
	if init.DalgonaNative == nil {
		c.t.Fatal("initialize did not negotiate dalgonaNative")
	}
	c.mu.Lock()
	c.streamID = init.DalgonaNative.StreamID
	c.mu.Unlock()
	return init
}

func nativeInitialize(version int, delivery string) map[string]any {
	return map[string]any{
		"clientInfo":   map[string]string{"name": "native-test", "version": "1"},
		"capabilities": map[string]any{"dalgonaNative": map[string]any{"version": version, "delivery": delivery}},
	}
}

func (c *nativeClient) read() {
	scanner := bufio.NewScanner(c.conn)
	scanner.Buffer(make([]byte, 64<<10), 16<<20)
	for scanner.Scan() {
		var m message
		if err := json.Unmarshal(bytes.Clone(scanner.Bytes()), &m); err != nil {
			c.t.Errorf("undecodable line: %v", err)
			continue
		}
		c.route(m)
	}
	if err := scanner.Err(); err != nil && !errors.Is(err, io.ErrClosedPipe) && !errors.Is(err, net.ErrClosed) {
		c.t.Errorf("native client read error: %v", err)
	}
}

func (c *nativeClient) route(m message) {
	switch m.Method {
	case "":
		c.mu.Lock()
		ch := c.waiting[string(m.ID)]
		c.mu.Unlock()
		if ch != nil {
			ch <- m
		}
	case rpc.MethodEvent:
		c.mu.Lock()
		event, err := rpc.ParseEvent(c.streamID, m.Params)
		if err == nil && event.First != c.ordinal+1 {
			err = errors.New("ordinal gap or overlap at " + strconv.FormatUint(event.First, 10) + " after " + strconv.FormatUint(c.ordinal, 10))
		}
		c.ordinal = event.Last
		c.mu.Unlock()
		if err != nil {
			c.t.Errorf("native stream broken: %v", err)
		}
		c.events <- event
	case rpc.MethodRequest:
		c.checkRequestOrdinal(m)
		c.requests <- m
	default:
		c.plain <- m
	}
}

func (c *nativeClient) checkRequestOrdinal(m message) {
	var header rpc.RequestParams
	if err := json.Unmarshal(m.Params, &header); err != nil {
		c.t.Errorf("decode native request: %v", err)
		return
	}
	ordinal, err := rpc.ParseDecimal(header.DalgonaEvent.Ordinal)
	c.mu.Lock()
	defer c.mu.Unlock()
	if err != nil || ordinal != c.ordinal+1 || header.DalgonaEvent.StreamID != c.streamID {
		c.t.Errorf("native request ordinal %s after %d on stream %s", header.DalgonaEvent.Ordinal, c.ordinal, header.DalgonaEvent.StreamID)
	}
	c.ordinal = ordinal
}

func (c *nativeClient) send(v any) {
	line, err := json.Marshal(v)
	if err != nil {
		c.t.Fatal(err)
	}
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	if _, err := c.conn.Write(append(line, '\n')); err != nil {
		c.t.Fatal(err)
	}
}

func (c *nativeClient) call(method string, params any) message {
	c.t.Helper()
	c.mu.Lock()
	c.next++
	id := strconv.Itoa(c.next)
	ch := make(chan message, 1)
	c.waiting[id] = ch
	c.mu.Unlock()
	raw, err := json.Marshal(params)
	if err != nil {
		c.t.Fatal(err)
	}
	c.send(message{ID: jsontext.Value(id), Method: method, Params: raw})
	select {
	case m := <-ch:
		return m
	case <-time.After(waitLimit):
		c.t.Fatalf("no response to %s", method)
		return message{}
	}
}

func (c *nativeClient) ok(method string, params any, out any) {
	c.t.Helper()
	m := c.call(method, params)
	if m.Error != nil {
		c.t.Fatalf("%s failed: %d %s", method, m.Error.Code, m.Error.Message)
	}
	if out == nil {
		return
	}
	if err := json.Unmarshal(m.Result, out); err != nil {
		c.t.Fatalf("decode %s result %s: %v", method, m.Result, err)
	}
	if strings.HasPrefix(method, "dalgona/") {
		if err := validateDalgona(c.t, dalgonaSchemaName(method, ".result.json"), m.Result); err != nil {
			c.t.Fatalf("%s result does not match its schema: %v", method, err)
		}
	}
}

func (c *nativeClient) failsWith(method string, params any, kind rpc.ErrorKind) *rpc.RPCError {
	c.t.Helper()
	m := c.call(method, params)
	if m.Error == nil {
		c.t.Fatalf("%s succeeded with %s, want %s", method, m.Result, kind)
	}
	data, ok := rpc.ErrorDataOf(m.Error)
	if !ok || data.Kind != kind || m.Error.Message != string(kind) {
		c.t.Fatalf("%s error = %d %q %s, want kind %s", method, m.Error.Code, m.Error.Message, m.Error.Data, kind)
	}
	return m.Error
}

func (c *nativeClient) awaitEvent(method string, match func(rpc.Event) bool) rpc.Event {
	c.t.Helper()
	deadline := time.After(waitLimit)
	for {
		select {
		case event := <-c.events:
			if event.Method == method && (match == nil || match(event)) {
				return event
			}
		case <-deadline:
			c.t.Fatalf("no %s event", method)
			return rpc.Event{}
		}
	}
}

func (c *nativeClient) startThread(r *rig) string {
	c.t.Helper()
	var started rpc.ThreadStartResponse
	c.ok("thread/start", map[string]any{"cwd": r.cwd}, &started)
	if started.Thread.ID == "" {
		c.t.Fatal("thread/start returned no thread id")
	}
	return started.Thread.ID
}

func (c *nativeClient) readContent(ref rpc.ContentReference) []byte {
	c.t.Helper()
	var out []byte
	for offset := "0"; ; {
		var chunk rpc.ContentChunk
		c.ok("dalgona/history/content/read", map[string]any{"source": ref.Source, "contentId": ref.ContentID, "offset": offset}, &chunk)
		data, err := base64.StdEncoding.DecodeString(chunk.DataBase64)
		if err != nil {
			c.t.Fatal(err)
		}
		out = append(out, data...)
		if chunk.Next == chunk.Total || chunk.Digest != ref.Digest {
			break
		}
		offset = chunk.Next
	}
	sum := sha256.Sum256(out)
	if hex.EncodeToString(sum[:]) != ref.Digest || strconv.Itoa(len(out)) != ref.Total {
		c.t.Fatalf("content %s read %d bytes with digest %x, reference says %s bytes %s", ref.ContentID, len(out), sum, ref.Total, ref.Digest)
	}
	return out
}

func eventItem(t *testing.T, event rpc.Event) rpc.ItemHeader {
	t.Helper()
	var params struct {
		Item rpc.ItemHeader `json:"item"`
	}
	if err := json.Unmarshal(event.Params, &params); err != nil {
		t.Fatalf("decode %s: %v", event.Method, err)
	}
	return params.Item
}

func contentField(header rpc.ItemHeader, field string) (rpc.ContentReference, bool) {
	for _, ref := range header.Contents {
		if ref.Field == field {
			return ref, true
		}
	}
	return rpc.ContentReference{}, false
}

func eventDelta(t *testing.T, event rpc.Event) (string, string) {
	t.Helper()
	var params struct {
		ItemID string `json:"itemId"`
		Delta  string `json:"delta"`
	}
	if err := json.Unmarshal(event.Params, &params); err != nil {
		t.Fatalf("decode %s: %v", event.Method, err)
	}
	return params.ItemID, params.Delta
}

func TestNativeStreamCarriesThreadNotificationsOnlyAsOrderedEvents(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	threadID := c.startThread(r)
	r.llm.push(llmReply{body: sseText("hello", " world")})
	c.ok("turn/start", prompt(threadID, "hi"), nil)
	streamed, completed := collectStreamedDeltas(t, c)
	assertNoLegacyThreadNotifications(t, c)
	if streamed != "hello world" {
		t.Fatalf("streamed deltas = %q", streamed)
	}
	checkCompletedTextRef(t, c, completed)
}

func collectStreamedDeltas(t *testing.T, c *nativeClient) (string, rpc.ItemHeader) {
	t.Helper()
	var streamed strings.Builder
	var completed rpc.ItemHeader
	for _, event := range c.eventsUntil("turn/completed") {
		switch event.Method {
		case "item/agentMessage/delta":
			_, delta := eventDelta(t, event)
			streamed.WriteString(delta)
		case "item/completed":
			if header := eventItem(t, event); header.Type == rpc.KindAgent {
				completed = header
			}
		}
	}
	return streamed.String(), completed
}

func checkCompletedTextRef(t *testing.T, c *nativeClient, completed rpc.ItemHeader) {
	t.Helper()
	ref, ok := contentField(completed, "text")
	if !ok || ref.Source.Kind != "live" || ref.Source.StreamID != c.streamID {
		t.Fatalf("completed agent header = %+v", completed)
	}
	if text := c.readContent(ref); string(text) != "hello world" {
		t.Fatalf("live content = %q", text)
	}
}

func assertNoLegacyThreadNotifications(t *testing.T, c *nativeClient) {
	t.Helper()
	for {
		select {
		case m := <-c.plain:
			if fieldOf(t, m.Params, "threadId") != "" {
				t.Fatalf("native connection received unwrapped thread notification %s", m.Method)
			}
		default:
			return
		}
	}
}

type deltaRecorder struct {
	mu   sync.Mutex
	text []byte
}

func TestSnapshotCutReconcilesActiveDeltasExactlyOnce(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	s, c := manualNativeServer(t, r)
	threadID := c.startThread(r)
	gate := make(chan struct{})
	t.Cleanup(func() { close(gate) })
	r.llm.push(llmReply{gate: gate, body: sseText("done")})
	c.ok("turn/start", prompt(threadID, "stream"), nil)
	c.awaitEvent("turn/started", nil)
	s.mu.Lock()
	w := s.watches[threadID]
	s.mu.Unlock()
	source := &deltaRecorder{}
	produced := make(chan struct{})
	go produceDeltas(w, source, produced)
	cuts := openSnapshotsUntil(t, c, threadID, produced)
	final, _ := s.conn.Cut(threadID)
	streamed := collectItemDeltas(t, c, "m1", final.Ordinal)
	midStream := 0
	for _, cut := range cuts {
		reconciled := append(bytes.Clone(cut.text), deltasAfter(t, streamed, cut.ordinal)...)
		if !bytes.Equal(reconciled, source.text) {
			t.Fatalf("snapshot at cut %d holds %d bytes; reconciled %d bytes, source has %d", cut.ordinal, len(cut.text), len(reconciled), len(source.text))
		}
		if len(cut.text) > 0 && len(cut.text) < len(source.text) {
			midStream++
		}
	}
	if midStream == 0 {
		t.Fatalf("none of %d snapshots landed while deltas were pending", len(cuts))
	}
}

func manualNativeServer(t *testing.T, r *rig) (*server, *nativeClient) {
	t.Helper()
	serverEnd, clientEnd := net.Pipe()
	s := &server{
		host: r.host, log: slog.New(slog.DiscardHandler), watches: map[string]*watch{}, pending: map[string]context.CancelFunc{}, threads: map[string]string{},
		refused: map[string]struct{}{}, logins: map[string]context.CancelFunc{}, fsWatches: map[string]*fsWatcher{},
		ctx: t.Context(), connection: newHandle(),
	}
	s.conn = rpc.NewConn(t.Context(), serverEnd, s.handle)
	c := newNativeClient(t, clientEnd)
	t.Cleanup(func() {
		_ = clientEnd.Close()
		_ = s.conn.Close()
		s.shutdown()
	})
	c.negotiate()
	return s, c
}

func produceDeltas(w *watch, source *deltaRecorder, done chan<- struct{}) {
	defer close(done)
	random := rand.New(rand.NewPCG(7, 11))
	for i := range 700 {
		delta := strings.Repeat(string(rune('a'+i%26))+"é한", 1+random.IntN(300))
		source.mu.Lock()
		source.text = append(source.text, delta...)
		source.mu.Unlock()
		w.trackDelta(agent.Update{Kind: agent.UpdateTextDelta, ItemID: "m1", Delta: delta})
		if i%25 == 0 {
			time.Sleep(time.Millisecond)
		}
	}
}

type capturedCut struct {
	ordinal uint64
	text    []byte
}

func openSnapshotsUntil(t *testing.T, c *nativeClient, threadID string, done <-chan struct{}) []capturedCut {
	t.Helper()
	var cuts []capturedCut
	previous := ""
	for finished := false; !finished; {
		select {
		case <-done:
			finished = true
		default:
		}
		params := map[string]any{"threadId": threadID}
		if previous != "" {
			params["replaceHistoryId"] = previous
		}
		var snap rpc.HistorySnapshot
		c.ok("dalgona/history/open", params, &snap)
		previous = snap.HistoryID
		cut, err := rpc.ParseDecimal(snap.CutOrdinal)
		if err != nil {
			t.Fatal(err)
		}
		cuts = append(cuts, capturedCut{ordinal: cut, text: activeText(t, c, snap, "m1")})
	}
	return cuts
}

func activeText(t *testing.T, c *nativeClient, snap rpc.HistorySnapshot, itemID string) []byte {
	t.Helper()
	var page rpc.HistoryPage
	c.ok("dalgona/history/page", map[string]any{"historyId": snap.HistoryID, "limit": 1}, &page)
	if len(page.Turns) != 1 || page.Turns[0].TurnID != snap.ActiveTurnID {
		t.Fatalf("newest page = %+v, want active turn %s", page.Turns, snap.ActiveTurnID)
	}
	for _, header := range page.Turns[0].Items {
		if header.ID != itemID {
			continue
		}
		ref, ok := contentField(header, "text")
		if !ok {
			t.Fatalf("active item header %+v has no text reference", header)
		}
		return c.readContent(ref)
	}
	return nil
}

func collectItemDeltas(t *testing.T, c *nativeClient, itemID string, final uint64) []rpc.Event {
	t.Helper()
	var out []rpc.Event
	deadline := time.After(waitLimit)
	for {
		select {
		case event := <-c.events:
			if isItemDelta(t, event, itemID) {
				out = append(out, event)
			}
			if event.Last >= final {
				return out
			}
		case <-deadline:
			t.Fatalf("stream did not reach ordinal %d", final)
			return nil
		}
	}
}

func isItemDelta(t *testing.T, event rpc.Event, itemID string) bool {
	if event.Method != "item/agentMessage/delta" {
		return false
	}
	id, _ := eventDelta(t, event)
	return id == itemID
}

func (c *nativeClient) eventsUntil(method string) []rpc.Event {
	c.t.Helper()
	var seen []rpc.Event
	deadline := time.After(waitLimit)
	for {
		select {
		case event := <-c.events:
			seen = append(seen, event)
			if event.Method == method {
				return seen
			}
		case <-deadline:
			c.t.Fatalf("no %s event after %d events", method, len(seen))
			return nil
		}
	}
}

func deltasAfter(t *testing.T, events []rpc.Event, cut uint64) []byte {
	t.Helper()
	var out []byte
	for _, event := range events {
		after, err := event.AfterCut(cut)
		if err != nil {
			t.Fatal(err)
		}
		if after {
			_, delta := eventDelta(t, event)
			out = append(out, delta...)
		}
	}
	return out
}

func TestFreshHistoryIncludesAlreadyStreamedActivePrefix(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	const first, last = "first half ", "second half"
	gate := make(chan struct{})
	unpause := sync.OnceFunc(func() { close(gate) })
	t.Cleanup(unpause)
	prefix := sseEvent("response.output_text.delta", `{"type":"response.output_text.delta","output_index":0,"delta":`+quoted(first)+`}`)
	r.llm.push(llmReply{prefix: prefix, body: sseText(last), gate: gate})
	original := r.connectNative(ServeOptions{})
	threadID := original.startThread(r)
	original.ok("turn/start", prompt(threadID, "stream across subscriptions"), nil)
	itemID, text := eventDelta(t, original.awaitEvent("item/agentMessage/delta", nil))
	if text != first {
		t.Fatalf("original stream sent %q, want %q", text, first)
	}

	reopened := r.connectNative(ServeOptions{})
	var snap rpc.HistorySnapshot
	reopened.ok("dalgona/history/open", map[string]string{"threadId": threadID}, &snap)
	if got := activeText(t, reopened, snap, itemID); string(got) != first {
		t.Fatalf("fresh snapshot prefix = %q, want %q", got, first)
	}
	unpause()
	postCut := reopened.awaitEvent("item/agentMessage/delta", nil)
	gotID, suffix := eventDelta(t, postCut)
	if gotID != itemID || suffix != last {
		t.Fatalf("new connection delta = %s %q, want %s %q", gotID, suffix, itemID, last)
	}
}
