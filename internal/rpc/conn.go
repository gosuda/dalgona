package rpc

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"strconv"
	"strings"
	"sync"
	"time"
)

type Handler func(context.Context, string, jsontext.Value) (jsontext.Value, error)

type ClientHandler interface {
	Notification(method string, params jsontext.Value)
	Request(ctx context.Context, method string, params jsontext.Value) (result any, err error)
}

const (
	ClientDispatchQueueSize = 256
	drainLimit              = 30 * time.Second
)

type Conn struct {
	io.ReadWriteCloser
	ctx            context.Context
	cancel         context.CancelFunc
	handler        Handler
	clientHandler  ClientHandler
	clientMessages chan envelope
	pending        map[string]chan response
	mu             sync.Mutex
	ids            uint64
	queue          *outQueue
	closed         chan struct{}
	once           sync.Once
	closeErr       error
	handlers       sync.WaitGroup
	remote         string
}

type (
	response struct {
		result jsontext.Value
		err    *wireError
	}
	wireError struct {
		Code    int            `json:"code"`
		Message string         `json:"message"`
		Data    jsontext.Value `json:"data,omitzero"`
	}
	envelope struct {
		JSONRPC string         `json:"jsonrpc,omitzero"`
		ID      jsontext.Value `json:"id,omitzero"`
		Method  string         `json:"method,omitzero"`
		Params  jsontext.Value `json:"params,omitzero"`
		Result  jsontext.Value `json:"result,omitzero"`
		Error   *wireError     `json:"error,omitzero"`
	}
)

func NewConn(ctx context.Context, rwc io.ReadWriteCloser, handler Handler) *Conn {
	c := newConnection(ctx, rwc, handler)
	c.start()
	return c
}

func newConnection(ctx context.Context, rwc io.ReadWriteCloser, handler Handler) *Conn {
	if ctx == nil {
		ctx = context.Background()
	}
	return &Conn{ReadWriteCloser: rwc, ctx: ctx, handler: handler, pending: make(map[string]chan response), queue: newOutQueue(), closed: make(chan struct{}), remote: peerName(rwc)}
}

func (c *Conn) start() {
	go c.writeLoop()
	go c.readLoop()
}

func Dial(rwc io.ReadWriteCloser, h ClientHandler) (*Client, error) {
	ctx, cancel := context.WithCancel(context.Background())
	c := newConnection(ctx, rwc, nil)
	c.cancel = cancel
	c.clientHandler = h
	c.clientMessages = make(chan envelope, ClientDispatchQueueSize)
	go c.clientDispatchLoop()
	c.start()
	return c, nil
}

func (c *Conn) Call(ctx context.Context, method string, params any, result any) error {
	idv := jsontext.Value(strconv.AppendUint(nil, c.nextID(), 10))
	return c.call(ctx, idv, method, params, result)
}

func (c *Conn) CallID(ctx context.Context, id string, method string, params any, result any) error {
	idv, err := json.Marshal(id)
	if err != nil {
		return fmt.Errorf("encode request id: %w", err)
	}
	return c.call(ctx, idv, method, params, result)
}

func (c *Conn) call(ctx context.Context, idv jsontext.Value, method string, params any, result any) error {
	ch := make(chan response, 1)
	key := string(idv)
	c.mu.Lock()
	c.pending[key] = ch
	c.mu.Unlock()
	pv, err := marshalParams(params)
	if err != nil {
		c.removePending(key)
		return err
	}
	if err := c.enqueue(envelope{JSONRPC: "2.0", ID: idv, Method: method, Params: pv}, false); err != nil {
		c.removePending(key)
		return err
	}
	select {
	case <-ctx.Done():
		c.removePending(key)
		return ctx.Err()
	case <-c.closed:
		c.removePending(key)
		return io.ErrClosedPipe
	case r := <-ch:
		if r.err != nil {
			return fmt.Errorf("rpc error %d: %w", r.err.Code, &RPCError{Code: r.err.Code, Message: r.err.Message, Data: r.err.Data})
		}
		if result == nil {
			return nil
		}
		return json.Unmarshal(r.result, result)
	}
}

func (c *Conn) Notify(method string, params any) error {
	pv, err := marshalParams(params)
	if err != nil {
		return err
	}
	return c.enqueue(envelope{JSONRPC: "2.0", Method: method, Params: pv}, true)
}

func (c *Conn) Close() error {
	c.once.Do(func() {
		if c.cancel != nil {
			c.cancel()
		}
		close(c.closed)
		if err := c.ReadWriteCloser.Close(); err != nil {
			c.closeErr = fmt.Errorf("close rpc connection: %w", err)
		}
		c.mu.Lock()
		c.pending = make(map[string]chan response)
		c.mu.Unlock()
	})
	return c.closeErr
}
func (c *Conn) nextID() uint64         { c.mu.Lock(); defer c.mu.Unlock(); c.ids++; return c.ids }
func (c *Conn) removePending(k string) { c.mu.Lock(); delete(c.pending, k); c.mu.Unlock() }
func marshalParams(v any) (jsontext.Value, error) {
	if v == nil {
		return nil, nil
	}
	return json.Marshal(v)
}

func (c *Conn) readLoop() {
	s := bufio.NewScanner(c)
	s.Buffer(make([]byte, 4096), 16<<20)
	for s.Scan() {
		var e envelope
		if err := json.Unmarshal(s.Bytes(), &e); err != nil {
			continue
		}
		c.dispatch(e)
	}
	if c.handler != nil {
		c.drainAfterRead()
	}
	if err := c.Close(); err != nil {
		slog.Debug("rpc: close after read failure", "error", err)
	}
}

func (c *Conn) dispatch(e envelope) {
	if e.Method == "" {
		c.dispatchResponse(e)
		return
	}
	if c.clientMessages != nil {
		select {
		case c.clientMessages <- e:
		case <-c.closed:
		}
		return
	}
	c.dispatchRequest(e)
}

func (c *Conn) dispatchResponse(e envelope) {
	if len(e.ID) == 0 {
		return
	}
	c.mu.Lock()
	ch := c.pending[string(e.ID)]
	delete(c.pending, string(e.ID))
	c.mu.Unlock()
	if ch != nil {
		ch <- response{result: e.Result, err: e.Error}
	}
}

func (c *Conn) dispatchRequest(e envelope) {
	if c.handler == nil {
		if len(e.ID) > 0 {
			c.replyError(e.ID, -32601, "Method not found")
		}
		return
	}
	c.handlers.Go(func() { c.handleRequest(e) })
}

func (c *Conn) handleRequest(e envelope) {
	result, handlerErr := c.handler(c.ctx, e.Method, e.Params)
	if len(e.ID) == 0 {
		return
	}
	if handlerErr != nil {
		code, message := -32603, handlerErr.Error()
		if coded, ok := errors.AsType[*RPCError](handlerErr); ok {
			code, message = coded.Code, coded.Message
		}
		c.replyError(e.ID, code, message)
		return
	}
	if enqueueErr := c.enqueue(envelope{JSONRPC: "2.0", ID: e.ID, Result: result}, false); enqueueErr != nil {
		c.abort(enqueueErr)
	}
}

func (c *Conn) clientDispatchLoop() {
	for {
		select {
		case e := <-c.clientMessages:
			c.handleClientMessage(e)
		case <-c.closed:
			return
		}
	}
}

func (c *Conn) handleClientMessage(e envelope) {
	if len(e.ID) == 0 {
		if c.clientHandler != nil {
			c.clientHandler.Notification(e.Method, e.Params)
		}
		return
	}
	c.handleClientRequest(e)
}

func (c *Conn) handleClientRequest(e envelope) {
	if c.clientHandler == nil {
		c.replyError(e.ID, -32601, "Method not found")
		return
	}
	result, handlerErr := c.clientHandler.Request(c.ctx, e.Method, e.Params)
	if handlerErr != nil {
		c.replyError(e.ID, -32601, handlerErr.Error())
		return
	}
	raw, err := json.Marshal(result)
	if err != nil {
		c.replyError(e.ID, -32603, err.Error())
		return
	}
	if enqueueErr := c.enqueue(envelope{JSONRPC: "2.0", ID: e.ID, Result: raw}, false); enqueueErr != nil {
		c.abort(enqueueErr)
	}
}

func (c *Conn) replyError(id jsontext.Value, code int, msg string) {
	if err := c.enqueue(envelope{JSONRPC: "2.0", ID: id, Error: &wireError{Code: code, Message: msg}}, false); err != nil {
		c.abort(err)
	}
}

func (c *Conn) abort(enqueueErr error) {
	if err := c.Close(); err != nil {
		slog.Debug("rpc: close after enqueue failure", "error", err, "enqueueError", enqueueErr)
		return
	}
	slog.Debug("rpc: closed after enqueue failure", "error", enqueueErr)
}

func (c *Conn) enqueue(e envelope, delta bool) error {
	b, err := json.Marshal(e)
	if err != nil {
		return err
	}
	return c.queue.push(b, e.Method, e.Params, delta, c.slowClose)
}

func (c *Conn) slowClose() error {
	slog.Error("rpc: closing slow client " + c.remote)
	if err := c.Close(); err != nil {
		return fmt.Errorf("close slow client: %w", err)
	}
	return nil
}

func peerName(rwc io.ReadWriteCloser) string {
	type adder interface{ RemoteAddr() net.Addr }
	if addr, ok := rwc.(adder); ok {
		if remote := addr.RemoteAddr(); remote != nil {
			return remote.String()
		}
	}
	return "unknown"
}

func (c *Conn) writeLoop() {
	for {
		item, ok := c.queue.pop(c.closed)
		if !ok {
			return
		}
		if item.flushed != nil {
			close(item.flushed)
			continue
		}
		b, err := item.line()
		if err != nil {
			c.abort(fmt.Errorf("encode queued response: %w", err))
			return
		}
		if _, writeErr := c.Write(append(b, '\n')); writeErr != nil {
			if closeErr := c.Close(); closeErr != nil {
				slog.Debug("rpc: close after write failure", "error", closeErr, "writeError", writeErr)
			}
			return
		}
	}
}

func (c *Conn) drainAfterRead() {
	handled := make(chan struct{})
	go func() {
		c.handlers.Wait()
		close(handled)
	}()
	timer := time.NewTimer(drainLimit)
	defer timer.Stop()
	select {
	case <-handled:
	case <-timer.C:
		slog.Warn("rpc: closing with unanswered requests after the peer stopped sending", "limit", drainLimit)
		return
	case <-c.closed:
		return
	}
	flushed := make(chan struct{})
	c.queue.mu.Lock()
	accepted := c.queue.append(queued{flushed: flushed})
	c.queue.mu.Unlock()
	if !accepted {
		return
	}
	select {
	case <-flushed:
	case <-timer.C:
	case <-c.closed:
	}
}

type (
	queued struct {
		b       []byte
		method  string
		delta   bool
		stream  *queuedDelta
		flushed chan struct{}
	}
	queuedDelta struct {
		itemID    string
		text      []byte
		params    jsontext.Value
		hasItemID bool
		hasText   bool
	}
	outQueue struct {
		mu    sync.Mutex
		items []queued
		wake  chan struct{}
	}
)

func newOutQueue() *outQueue { return &outQueue{wake: make(chan struct{}, 1)} }
func (q *outQueue) push(b []byte, method string, params jsontext.Value, delta bool, closeFn func() error) error {
	item := newQueued(b, method, params, delta)
	q.mu.Lock()
	if q.merge(item) {
		q.mu.Unlock()
		return nil
	}
	accepted := q.append(item)
	q.mu.Unlock()
	if !accepted {
		return errors.Join(io.ErrShortBuffer, closeFn())
	}
	return nil
}

func newQueued(b []byte, method string, params jsontext.Value, delta bool) queued {
	delta = delta && isDeltaMethod(method)
	item := queued{b: b, method: method, delta: delta}
	if !delta {
		return item
	}
	stream, err := parseDelta(params)
	if err == nil {
		item.stream = &stream
	}
	return item
}

func (q *outQueue) merge(item queued) bool {
	return len(q.items) >= 1024 && item.stream != nil && mergeDelta(q.items, item.method, item.stream)
}

func (q *outQueue) append(item queued) bool {
	if len(q.items) >= 4096 {
		return false
	}
	q.items = append(q.items, item)
	select {
	case q.wake <- struct{}{}:
	default:
	}
	return true
}

func (q *outQueue) pop(closed <-chan struct{}) (queued, bool) {
	for {
		q.mu.Lock()
		if len(q.items) > 0 {
			item := q.items[0]
			q.items[0] = queued{}
			q.items = q.items[1:]
			q.mu.Unlock()
			return item, true
		}
		q.mu.Unlock()
		select {
		case <-closed:
			return queued{}, false
		case <-q.wake:
		}
	}
}

func isDeltaMethod(method string) bool {
	switch method {
	case "item/agentMessage/delta", "item/reasoning/textDelta", "item/commandExecution/outputDelta":
		return true
	default:
		return false
	}
}

func mergeDelta(items []queued, method string, next *queuedDelta) bool {
	for i := len(items) - 1; i >= 0; i-- {
		current := items[i].stream
		if items[i].method != method || current == nil || current.itemID != next.itemID {
			continue
		}
		current.text = append(current.text, next.text...)
		return true
	}
	return false
}

func parseDelta(params jsontext.Value) (queuedDelta, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(params))
	opening, err := decoder.ReadToken()
	if err != nil {
		return queuedDelta{}, fmt.Errorf("read delta object: %w", err)
	}
	if opening.Kind() != '{' {
		return queuedDelta{}, fmt.Errorf("delta params must be an object")
	}
	result := queuedDelta{params: params}
	for decoder.PeekKind() != '}' {
		if err := result.readMember(decoder); err != nil {
			return queuedDelta{}, err
		}
	}
	if _, err := decoder.ReadToken(); err != nil {
		return queuedDelta{}, fmt.Errorf("close delta object: %w", err)
	}
	if !result.hasItemID || !result.hasText {
		return queuedDelta{}, fmt.Errorf("delta params need string itemId and delta")
	}
	return result, nil
}

func (d *queuedDelta) readMember(decoder *jsontext.Decoder) error {
	name, err := decoder.ReadToken()
	if err != nil {
		return fmt.Errorf("read delta member: %w", err)
	}
	member := strings.Clone(name.String())
	switch member {
	case "itemId":
		d.itemID, err = readJSONString(decoder)
		d.hasItemID = err == nil
	case "delta":
		var text string
		text, err = readJSONString(decoder)
		d.text = []byte(text)
		d.hasText = err == nil
	default:
		err = decoder.SkipValue()
	}
	if err != nil {
		return fmt.Errorf("read delta member %q: %w", member, err)
	}
	return nil
}

func readJSONString(decoder *jsontext.Decoder) (string, error) {
	token, err := decoder.ReadToken()
	if err != nil {
		return "", fmt.Errorf("read string: %w", err)
	}
	if token.Kind() != '"' {
		return "", fmt.Errorf("expected a JSON string")
	}
	return strings.Clone(token.String()), nil
}

func (q *queued) line() ([]byte, error) {
	if q.stream == nil {
		return q.b, nil
	}
	var out bytes.Buffer
	encoder := jsontext.NewEncoder(&out)
	if err := writeDeltaEnvelope(encoder, q.method, q.stream); err != nil {
		return nil, fmt.Errorf("write delta notification: %w", err)
	}
	return bytes.TrimSuffix(out.Bytes(), []byte("\n")), nil
}

func writeDeltaEnvelope(encoder *jsontext.Encoder, method string, delta *queuedDelta) error {
	tokens := [...]jsontext.Token{
		jsontext.BeginObject,
		jsontext.String("jsonrpc"), jsontext.String("2.0"),
		jsontext.String("method"), jsontext.String(method),
		jsontext.String("params"),
	}
	for _, token := range tokens {
		if err := writeJSONToken(encoder, token); err != nil {
			return err
		}
	}
	if err := writeDeltaParams(encoder, delta); err != nil {
		return err
	}
	return writeJSONToken(encoder, jsontext.EndObject)
}

func writeDeltaParams(encoder *jsontext.Encoder, delta *queuedDelta) error {
	decoder := jsontext.NewDecoder(bytes.NewReader(delta.params))
	opening, err := decoder.ReadToken()
	if err != nil {
		return fmt.Errorf("read params: %w", err)
	}
	if writeErr := writeJSONToken(encoder, opening); writeErr != nil {
		return writeErr
	}
	for decoder.PeekKind() != '}' {
		if writeErr := writeDeltaMember(encoder, decoder, delta.text); writeErr != nil {
			return writeErr
		}
	}
	closing, err := decoder.ReadToken()
	if err != nil {
		return fmt.Errorf("close params: %w", err)
	}
	return writeJSONToken(encoder, closing)
}

func writeDeltaMember(encoder *jsontext.Encoder, decoder *jsontext.Decoder, text []byte) error {
	name, err := decoder.ReadToken()
	if err != nil {
		return fmt.Errorf("read member name: %w", err)
	}
	if writeErr := writeJSONToken(encoder, name); writeErr != nil {
		return writeErr
	}
	if name.String() == "delta" {
		if skipErr := decoder.SkipValue(); skipErr != nil {
			return fmt.Errorf("skip original delta: %w", skipErr)
		}
		return writeJSONToken(encoder, jsontext.String(string(text)))
	}
	value, err := decoder.ReadValue()
	if err != nil {
		return fmt.Errorf("read member value: %w", err)
	}
	if err := encoder.WriteValue(value); err != nil {
		return fmt.Errorf("write member value: %w", err)
	}
	return nil
}

func writeJSONToken(encoder *jsontext.Encoder, token jsontext.Token) error {
	if err := encoder.WriteToken(token); err != nil {
		return fmt.Errorf("write JSON token: %w", err)
	}
	return nil
}
