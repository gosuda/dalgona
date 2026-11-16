package acp

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"strconv"
	"sync"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	codeParse          = -32700
	codeInvalidRequest = -32600
	codeMethodNotFound = -32601
	codeInvalidParams  = -32602
	codeInternal       = -32603
)

type wireError struct {
	Code    int            `json:"code"`
	Message string         `json:"message"`
	Data    jsontext.Value `json:"data,omitzero"`
}

type message struct {
	JSONRPC string         `json:"jsonrpc,omitzero"`
	ID      jsontext.Value `json:"id,omitzero"`
	Method  string         `json:"method,omitzero"`
	Params  jsontext.Value `json:"params,omitzero"`
	Result  jsontext.Value `json:"result,omitzero"`
	Error   *wireError     `json:"error,omitzero"`
}

type peer struct {
	out     io.Writer
	writeMu sync.Mutex
	mu      sync.Mutex
	nextID  uint64
	calls   map[string]chan message
	done    chan struct{}
	closed  bool
}

func newPeer(out io.Writer) *peer {
	return &peer{out: out, calls: make(map[string]chan message), done: make(chan struct{})}
}

func rpcError(code int, text string) *rpc.RPCError {
	return &rpc.RPCError{Code: code, Message: text}
}

func coded(err error) *rpc.RPCError {
	if wire, ok := errors.AsType[*rpc.RPCError](err); ok {
		return wire
	}
	return rpcError(codeInternal, err.Error())
}

func (p *peer) send(m message) error {
	m.JSONRPC = "2.0"
	line, err := json.Marshal(m)
	if err != nil {
		return fmt.Errorf("encode message: %w", err)
	}
	line = append(line, '\n')
	p.writeMu.Lock()
	defer p.writeMu.Unlock()
	select {
	case <-p.done:
		return io.ErrClosedPipe
	default:
	}
	if _, err = p.out.Write(line); err != nil {
		return fmt.Errorf("write message: %w", err)
	}
	return nil
}

func (p *peer) notify(method string, params jsontext.Value) error {
	return p.send(message{Method: method, Params: params})
}

func (p *peer) replyResult(id jsontext.Value, result any) error {
	if result == nil {
		result = struct{}{}
	}
	raw, err := json.Marshal(result)
	if err != nil {
		return p.replyError(id, fmt.Errorf("encode result: %w", err))
	}
	return p.send(message{ID: id, Result: raw})
}

func (p *peer) replyError(id jsontext.Value, err error) error {
	if len(id) == 0 {
		id = jsontext.Value("null")
	}
	wire := coded(err)
	return p.send(message{ID: id, Error: &wireError{Code: wire.Code, Message: wire.Message, Data: wire.Data}})
}

func (p *peer) call(ctx context.Context, method string, params, result any) error {
	raw, err := json.Marshal(params)
	if err != nil {
		return fmt.Errorf("encode %s params: %w", method, err)
	}
	id, ch, err := p.register()
	if err != nil {
		return err
	}
	if err = p.send(message{ID: id, Method: method, Params: raw}); err != nil {
		p.forget(id)
		return err
	}
	select {
	case m := <-ch:
		return decodeReply(method, m, result)
	case <-ctx.Done():
		p.forget(id)
		return ctx.Err()
	case <-p.done:
		p.forget(id)
		return io.ErrClosedPipe
	}
}

func decodeReply(method string, m message, result any) error {
	if m.Error != nil {
		return fmt.Errorf("editor %s: %w", method, &rpc.RPCError{Code: m.Error.Code, Message: m.Error.Message, Data: m.Error.Data})
	}
	if result == nil {
		return nil
	}
	if err := json.Unmarshal(m.Result, result); err != nil {
		return fmt.Errorf("decode %s result: %w", method, err)
	}
	return nil
}

func (p *peer) register() (jsontext.Value, chan message, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.closed {
		return nil, nil, io.ErrClosedPipe
	}
	p.nextID++
	id := jsontext.Value(strconv.AppendUint(nil, p.nextID, 10))
	ch := make(chan message, 1)
	p.calls[string(id)] = ch
	return id, ch, nil
}

func (p *peer) forget(id jsontext.Value) {
	p.mu.Lock()
	delete(p.calls, string(id))
	p.mu.Unlock()
}

func (p *peer) resolve(m message) {
	p.mu.Lock()
	ch := p.calls[string(m.ID)]
	delete(p.calls, string(m.ID))
	p.mu.Unlock()
	if ch != nil {
		ch <- m
	}
}

func (p *peer) shutdown() {
	p.mu.Lock()
	defer p.mu.Unlock()
	if !p.closed {
		p.closed = true
		close(p.done)
	}
}

func readMembers(raw jsontext.Value, member func(string, *jsontext.Decoder) error) error {
	d := jsontext.NewDecoder(bytes.NewReader(raw))
	open, err := d.ReadToken()
	if err != nil {
		return fmt.Errorf("read params: %w", err)
	}
	if open.Kind() != '{' {
		return errors.New("params are not an object")
	}
	for d.PeekKind() != '}' {
		var name jsontext.Token
		if name, err = d.ReadToken(); err != nil {
			return fmt.Errorf("read member name: %w", err)
		}
		if err = member(name.String(), d); err != nil {
			return err
		}
	}
	if _, err = d.ReadToken(); err != nil {
		return fmt.Errorf("read params end: %w", err)
	}
	return nil
}

func readString(d *jsontext.Decoder) (string, error) {
	tok, err := d.ReadToken()
	if err != nil {
		return "", fmt.Errorf("read string: %w", err)
	}
	if tok.Kind() != '"' {
		return "", errors.New("member is not a string")
	}
	return tok.String(), nil
}
