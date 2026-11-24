package rpc

import (
	"bytes"
	"cmp"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"slices"
	"strconv"
	"sync"
	"unicode/utf8"
)

const ordinalSlack = 48

type Spill func(threadID string, ordinal uint64, params jsontext.Value) (ContentReference, error)

type orderedStream struct {
	id       string
	spill    Spill
	mu       sync.Mutex
	ordinal  uint64
	requests map[string]*RequestDetail
}

type RequestDetail struct {
	ThreadID string
	Method   string
	Ordinal  uint64
	Params   []byte
	Digest   string
}

type Cut struct {
	Ordinal  uint64
	Requests []PendingRequest
}

type queuedEvent struct {
	stream  string
	thread  string
	method  string
	first   uint64
	last    uint64
	delta   queuedDelta
	base    int
	escaped int
	sealed  bool
}

func (c *Conn) OpenStream(streamID string, spill Spill) {
	c.queue.mu.Lock()
	c.queue.byteLimit = NativeQueueBytes
	c.queue.mu.Unlock()
	c.cancelAccepted.Store(true)
	c.stream.Store(&orderedStream{id: streamID, spill: spill, requests: map[string]*RequestDetail{}})
}

func (c *Conn) Publish(threadID, method string, params jsontext.Value) (uint64, error) {
	if stream := c.stream.Load(); stream != nil && threadID != "" {
		return stream.publish(c, threadID, method, params)
	}
	return 0, c.enqueue(envelope{JSONRPC: "2.0", Method: method, Params: params}, true)
}

func (c *Conn) Cut(threadID string) (Cut, bool) {
	stream := c.stream.Load()
	if stream == nil {
		return Cut{}, false
	}
	stream.mu.Lock()
	defer stream.mu.Unlock()
	c.queue.seal()
	return Cut{Ordinal: stream.ordinal, Requests: stream.threadRequests(threadID)}, true
}

func (c *Conn) PendingRequests(threadID string) []PendingRequest {
	stream := c.stream.Load()
	if stream == nil {
		return nil
	}
	stream.mu.Lock()
	defer stream.mu.Unlock()
	return stream.threadRequests(threadID)
}

func (s *orderedStream) threadRequests(threadID string) []PendingRequest {
	ids := make([]string, 0, len(s.requests))
	for id, detail := range s.requests {
		if detail.ThreadID == threadID {
			ids = append(ids, id)
		}
	}
	slices.SortFunc(ids, func(a, b string) int { return cmp.Compare(s.requests[a].Ordinal, s.requests[b].Ordinal) })
	requests := make([]PendingRequest, 0, len(ids))
	for _, id := range ids {
		requests = append(requests, s.requests[id].pending(id))
	}
	return requests
}

func (c *Conn) PendingRequest(requestID string) (RequestDetail, bool) {
	stream := c.stream.Load()
	if stream == nil {
		return RequestDetail{}, false
	}
	stream.mu.Lock()
	defer stream.mu.Unlock()
	detail, ok := stream.requests[requestID]
	if !ok {
		return RequestDetail{}, false
	}
	return *detail, true
}

func (d *RequestDetail) pending(requestID string) PendingRequest {
	return PendingRequest{RequestID: requestID, Method: d.Method, Details: ContentSummary{Total: FormatDecimal(uint64(len(d.Params))), Digest: d.Digest}}
}

func (s *orderedStream) publish(c *Conn, threadID, method string, params jsontext.Value) (uint64, error) {
	if isDeltaMethod(method) {
		if delta, err := parseDelta(params); err == nil {
			return s.publishDelta(c, threadID, method, delta)
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.ordinal++
	event := EventParams{StreamID: s.id, FirstOrdinal: FormatDecimal(s.ordinal), LastOrdinal: FormatDecimal(s.ordinal), ThreadID: threadID, Method: method, Params: params}
	line, err := eventLine(event)
	if err == nil && len(line) > EventEnvelopeLimit {
		line, err = s.spilledLine(event, s.ordinal)
	}
	if err != nil {
		c.abort(err)
		return 0, err
	}
	return s.ordinal, c.queue.push(line, MethodEvent, nil, false, c.slowClose)
}

func (s *orderedStream) spilledLine(event EventParams, ordinal uint64) ([]byte, error) {
	if s.spill == nil {
		return nil, errors.New("rpc: oversized event has no spill store")
	}
	ref, err := s.spill(event.ThreadID, ordinal, event.Params)
	if err != nil {
		return nil, fmt.Errorf("rpc: spill oversized %s event: %w", event.Method, err)
	}
	event.Params, event.ParamsContent = nil, &ref
	return eventLine(event)
}

func eventLine(event EventParams) ([]byte, error) {
	params, err := json.Marshal(event)
	if err != nil {
		return nil, fmt.Errorf("rpc: encode %s event: %w", event.Method, err)
	}
	line, err := json.Marshal(envelope{JSONRPC: "2.0", Method: MethodEvent, Params: params})
	if err != nil {
		return nil, fmt.Errorf("rpc: encode %s envelope: %w", event.Method, err)
	}
	return line, nil
}

func (s *orderedStream) publishDelta(c *Conn, threadID, method string, delta queuedDelta) (uint64, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	template := &queuedEvent{stream: s.id, thread: threadID, method: method, first: s.ordinal + 1, last: s.ordinal + 1, delta: queuedDelta{itemID: delta.itemID, params: delta.params}}
	empty, err := template.line()
	if err != nil {
		c.abort(err)
		return 0, err
	}
	base := len(empty) + ordinalSlack
	for _, chunk := range splitText(delta.text, TextDeltaLimit, EventEnvelopeLimit-base) {
		s.ordinal++
		event := *template
		event.first, event.last = s.ordinal, s.ordinal
		event.delta.text, event.base, event.escaped = chunk.text, base, chunk.escaped
		if err := c.queue.pushEvent(&event, c.slowClose); err != nil {
			return 0, err
		}
	}
	return s.ordinal, nil
}

func (s *orderedStream) request(c *Conn, idv jsontext.Value, threadID, method string, params jsontext.Value) (func(), error) {
	requestID := requestIDText(idv)
	if requestID == "" || len(requestID) > IDLimit {
		return nil, fmt.Errorf("rpc: request id %q cannot be wrapped", requestID)
	}
	sum := sha256.Sum256(params)
	detail := &RequestDetail{ThreadID: threadID, Method: method, Params: bytes.Clone(params), Digest: hex.EncodeToString(sum[:])}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.ordinal++
	detail.Ordinal = s.ordinal
	header := RequestParams{
		Method:       method,
		Details:      RequestDetails{RequestID: requestID, Total: FormatDecimal(uint64(len(params))), Digest: detail.Digest},
		DalgonaEvent: EventPosition{StreamID: s.id, Ordinal: FormatDecimal(s.ordinal), ThreadID: threadID},
	}
	raw, err := json.Marshal(header)
	if err == nil && len(raw) > RequestHeaderLimit {
		err = fmt.Errorf("rpc: request header for %s exceeds %d bytes", requestID, RequestHeaderLimit)
	}
	var line []byte
	if err == nil {
		line, err = json.Marshal(envelope{JSONRPC: "2.0", ID: idv, Method: MethodRequest, Params: raw})
	}
	if err != nil {
		c.abort(err)
		return nil, err
	}
	s.requests[requestID] = detail
	if err := c.queue.push(line, MethodRequest, nil, false, c.slowClose); err != nil {
		delete(s.requests, requestID)
		return nil, err
	}
	return func() { s.release(requestID, detail) }, nil
}

func (s *orderedStream) release(requestID string, detail *RequestDetail) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.requests[requestID] == detail {
		delete(s.requests, requestID)
	}
}

func requestIDText(idv jsontext.Value) string {
	var id string
	if json.Unmarshal(idv, &id) == nil {
		return id
	}
	return string(idv)
}

func threadOf(method string, params jsontext.Value) string {
	if len(params) == 0 {
		return ""
	}
	member := "threadId"
	if method == "thread/started" {
		member = "thread"
	}
	var threadID string
	readID := func(name string, d *jsontext.Decoder) error {
		if name != "id" {
			return d.SkipValue()
		}
		id, err := ReadString(d)
		threadID = id
		return err
	}
	err := ReadMembers(jsontext.NewDecoder(bytes.NewReader(params)), func(name string, d *jsontext.Decoder) error {
		switch {
		case name != member:
			return d.SkipValue()
		case member == "thread":
			return ReadMembers(d, readID)
		}
		return readID("id", d)
	})
	if err != nil {
		return ""
	}
	return threadID
}

type textChunk struct {
	text    []byte
	escaped int
}

func splitText(text []byte, rawLimit, escapedLimit int) []textChunk {
	if len(text) == 0 {
		return []textChunk{{text: text}}
	}
	var chunks []textChunk
	start, escaped := 0, 0
	for i := 0; i < len(text); {
		_, size := utf8.DecodeRune(text[i:])
		width := escapedWidth(text[i : i+size])
		if i > start && (i+size-start > rawLimit || escaped+width > escapedLimit) {
			chunks = append(chunks, textChunk{text: text[start:i:i], escaped: escaped})
			start, escaped = i, 0
		}
		escaped += width
		i += size
	}
	return append(chunks, textChunk{text: text[start:len(text):len(text)], escaped: escaped})
}

func escapedWidth(r []byte) int {
	if len(r) != 1 {
		return multiByteWidth(r)
	}
	return singleByteWidth(r[0])
}

func multiByteWidth(r []byte) int {
	if len(r) == 3 && r[0] == 0xE2 && r[1] == 0x80 && (r[2] == 0xA8 || r[2] == 0xA9) {
		return 6
	}
	return len(r)
}

func singleByteWidth(b byte) int {
	if singleEscape(b) {
		return 2
	}
	if b < 0x20 || b >= utf8.RuneSelf {
		return 6
	}
	return 1
}

func singleEscape(b byte) bool {
	switch b {
	case '"', '\\', '\b', '\f', '\n', '\r', '\t':
		return true
	}
	return false
}

func (e *queuedEvent) joins(next *queuedEvent) bool {
	return !e.sealed && e.thread == next.thread && e.method == next.method && e.delta.itemID == next.delta.itemID &&
		e.last+1 == next.first && len(e.delta.text)+len(next.delta.text) <= TextDeltaLimit &&
		e.base+e.escaped+next.escaped <= EventEnvelopeLimit
}

func (e *queuedEvent) size() int { return e.base + e.escaped }

func (e *queuedEvent) line() ([]byte, error) {
	var out bytes.Buffer
	encoder := jsontext.NewEncoder(&out)
	tokens := [...]jsontext.Token{
		jsontext.BeginObject,
		jsontext.String("jsonrpc"), jsontext.String("2.0"),
		jsontext.String("method"), jsontext.String(MethodEvent),
		jsontext.String("params"), jsontext.BeginObject,
		jsontext.String("streamId"), jsontext.String(e.stream),
		jsontext.String("firstOrdinal"), jsontext.String(strconv.FormatUint(e.first, 10)),
		jsontext.String("lastOrdinal"), jsontext.String(strconv.FormatUint(e.last, 10)),
		jsontext.String("threadId"), jsontext.String(e.thread),
		jsontext.String("method"), jsontext.String(e.method),
		jsontext.String("params"),
	}
	for _, token := range tokens {
		if err := writeJSONToken(encoder, token); err != nil {
			return nil, err
		}
	}
	if err := writeDeltaParams(encoder, &e.delta); err != nil {
		return nil, fmt.Errorf("write delta event: %w", err)
	}
	for _, token := range [...]jsontext.Token{jsontext.EndObject, jsontext.EndObject} {
		if err := writeJSONToken(encoder, token); err != nil {
			return nil, err
		}
	}
	return bytes.TrimSuffix(out.Bytes(), []byte("\n")), nil
}

func (q *outQueue) pushEvent(event *queuedEvent, closeFn func() error) error {
	q.mu.Lock()
	if n := len(q.items); n > 0 && q.items[n-1].event != nil && q.items[n-1].event.joins(event) {
		tail := q.items[n-1].event
		tail.delta.text = append(tail.delta.text, event.delta.text...)
		tail.escaped += event.escaped
		tail.last = event.last
		q.bytes += event.escaped
		q.items[n-1].size += event.escaped
		over := q.byteLimit > 0 && q.bytes > q.byteLimit
		q.mu.Unlock()
		if over {
			return errors.Join(io.ErrShortBuffer, closeFn())
		}
		return nil
	}
	accepted := q.append(queued{method: MethodEvent, event: event, size: event.size()})
	q.mu.Unlock()
	if !accepted {
		return errors.Join(io.ErrShortBuffer, closeFn())
	}
	return nil
}

func (q *outQueue) seal() {
	q.mu.Lock()
	defer q.mu.Unlock()
	if n := len(q.items); n > 0 && q.items[n-1].event != nil {
		q.items[n-1].event.sealed = true
	}
}
