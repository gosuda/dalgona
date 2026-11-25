package runner

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"sync"
	"time"
)

const (
	servedTerminalBuffer = 1 << 20
	servedTerminalPage   = 256 << 10
	servedTerminalInput  = 64 << 10
	servedTerminalIdle   = 2 * time.Minute
	servedTerminalChunk  = 32 << 10
)

var servedTerminalWriteTimeout = 5 * time.Second

var errTerminalWriteTimeout = errors.New("runner: terminal write timed out")

type writeRequest struct {
	data []byte
	done chan writeResult
}

type writeResult struct {
	written int
	err     error
}
type servedTerminal struct {
	term   Terminal
	mu     sync.Mutex
	space  *sync.Cond
	buf    []byte
	start  int64
	ended  bool
	closed bool
	idle   *time.Timer
	writes chan writeRequest
	stop   chan struct{}
}

var servedTerminals = struct {
	mu   sync.Mutex
	byID map[string]*servedTerminal
}{byID: map[string]*servedTerminal{}}

func rpcTerminalOpen(ctx context.Context, local *Local, params jsontext.Value) (jsontext.Value, error) {
	spec, err := decodeTerminalSpec(params)
	if err != nil {
		return nil, err
	}
	if serverSandboxFloor() == SandboxWorkspaceWrite && spec.Sandbox.Mode != SandboxWorkspaceWrite {
		spec.Sandbox = Sandbox{Mode: SandboxWorkspaceWrite, WritableRoots: []string{spec.Dir}}
	}
	term, err := local.OpenTerminal(ctx, spec)
	if err != nil {
		return nil, err
	}
	id, err := servedTerminalID()
	if err != nil {
		return nil, errors.Join(err, term.Signal(SignalKill), term.Close())
	}
	served := &servedTerminal{term: term, writes: make(chan writeRequest, 1), stop: make(chan struct{})}
	served.space = sync.NewCond(&served.mu)
	served.idle = time.AfterFunc(servedTerminalIdle, func() { releaseServedTerminal(id) })
	servedTerminals.mu.Lock()
	servedTerminals.byID[id] = served
	servedTerminals.mu.Unlock()
	go served.pump()
	go served.serveWrites()
	return encodeTerminalID(id)
}

func servedTerminalID() (string, error) {
	var raw [16]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return "", fmt.Errorf("runner: terminal id: %w", err)
	}
	return hex.EncodeToString(raw[:]), nil
}

func (t *servedTerminal) pump() {
	chunk := make([]byte, servedTerminalChunk)
	for {
		t.mu.Lock()
		for len(t.buf) >= servedTerminalBuffer && !t.closed {
			t.space.Wait()
		}
		if t.closed {
			t.mu.Unlock()
			return
		}
		free := servedTerminalBuffer - len(t.buf)
		t.mu.Unlock()
		n, err := t.term.Read(chunk[:min(len(chunk), free)])
		if !t.retain(chunk[:n]) || err != nil {
			t.mu.Lock()
			t.ended = true
			t.mu.Unlock()
			return
		}
	}
}

func (t *servedTerminal) retain(data []byte) bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	for len(t.buf) >= servedTerminalBuffer && !t.closed {
		t.space.Wait()
	}
	t.buf = append(t.buf, data...)
	return !t.closed
}

func (t *servedTerminal) page(offset int64) (Output, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	end := t.start + int64(len(t.buf))
	if offset < t.start || offset > end {
		return Output{}, errInvalidParams
	}
	t.buf = t.buf[offset-t.start:]
	t.start = offset
	t.space.Broadcast()
	size := min(len(t.buf), servedTerminalPage)
	out := Output{Data: bytes.Clone(t.buf[:size]), Next: offset + int64(size), Alive: !t.ended || size < len(t.buf)}
	select {
	case <-t.term.Done():
		exit := t.term.Exit()
		out.Exit = &exit
	default:
	}
	return out, nil
}

func lookupServedTerminal(params jsontext.Value) (*servedTerminal, map[string]jsontext.Value, error) {
	fields, err := paramsFields(params)
	if err != nil {
		return nil, nil, err
	}
	id, err := fieldString(fields, "terminalId")
	if err != nil {
		return nil, nil, err
	}
	servedTerminals.mu.Lock()
	served, ok := servedTerminals.byID[id]
	servedTerminals.mu.Unlock()
	if !ok {
		return nil, nil, ErrProcessNotFound
	}
	served.idle.Reset(servedTerminalIdle)
	return served, fields, nil
}

func releaseServedTerminal(id string) {
	servedTerminals.mu.Lock()
	served, ok := servedTerminals.byID[id]
	delete(servedTerminals.byID, id)
	servedTerminals.mu.Unlock()
	if !ok {
		return
	}
	served.idle.Stop()
	served.mu.Lock()
	served.closed = true
	served.space.Broadcast()
	served.mu.Unlock()
	close(served.stop)
	select {
	case <-served.term.Done():
	default:
		_ = served.term.Signal(SignalKill)
	}
	_ = served.term.Close()
}

func (t *servedTerminal) serveWrites() {
	for {
		select {
		case <-t.stop:
			return
		case req := <-t.writes:
			n, err := t.term.Write(req.data)
			req.done <- writeResult{written: n, err: err}
		}
	}
}

func rpcTerminalRead(_ context.Context, _ *Local, params jsontext.Value) (jsontext.Value, error) {
	served, fields, err := lookupServedTerminal(params)
	if err != nil {
		return nil, err
	}
	offset, err := fieldInt(fields, "offset")
	if err != nil {
		return nil, err
	}
	out, err := served.page(offset)
	if err != nil {
		return nil, err
	}
	return encodeOutput(out)
}

func rpcTerminalWrite(_ context.Context, _ *Local, params jsontext.Value) (jsontext.Value, error) {
	served, fields, err := lookupServedTerminal(params)
	if err != nil {
		return nil, err
	}
	encoded, err := fieldString(fields, "dataBase64")
	if err != nil {
		return nil, err
	}
	data, err := base64.StdEncoding.DecodeString(encoded)
	if err != nil || len(data) > servedTerminalInput {
		return nil, errInvalidParams
	}
	return served.write(data)
}

func (t *servedTerminal) write(data []byte) (jsontext.Value, error) {
	req := writeRequest{data: data, done: make(chan writeResult, 1)}
	select {
	case t.writes <- req:
	case <-time.After(servedTerminalWriteTimeout):
		return nil, errTerminalWriteTimeout
	}
	select {
	case res := <-req.done:
		return encodeTerminalWritten(int64(res.written), res.err)
	case <-time.After(servedTerminalWriteTimeout):
		return nil, errTerminalWriteTimeout
	}
}

func rpcTerminalResize(_ context.Context, _ *Local, params jsontext.Value) (jsontext.Value, error) {
	served, fields, err := lookupServedTerminal(params)
	if err != nil {
		return nil, err
	}
	columns, err := fieldInt(fields, "columns")
	if err != nil {
		return nil, err
	}
	rows, err := fieldInt(fields, "rows")
	if err != nil {
		return nil, err
	}
	if err := served.term.Resize(int(columns), int(rows)); err != nil {
		return nil, err
	}
	return emptyObject(), nil
}

func rpcTerminalSignal(_ context.Context, _ *Local, params jsontext.Value) (jsontext.Value, error) {
	served, fields, err := lookupServedTerminal(params)
	if err != nil {
		return nil, err
	}
	name, err := fieldString(fields, "signal")
	if err != nil {
		return nil, err
	}
	signal, err := decodeSignalName(name)
	if err != nil {
		return nil, err
	}
	if err := served.term.Signal(signal); err != nil {
		return nil, err
	}
	return emptyObject(), nil
}

func rpcTerminalClose(_ context.Context, _ *Local, params jsontext.Value) (jsontext.Value, error) {
	fields, err := paramsFields(params)
	if err != nil {
		return nil, err
	}
	id, err := fieldString(fields, "terminalId")
	if err != nil {
		return nil, err
	}
	releaseServedTerminal(id)
	return emptyObject(), nil
}

func decodeTerminalSpec(params jsontext.Value) (TerminalSpec, error) {
	fields, err := paramsFields(params)
	if err != nil {
		return TerminalSpec{}, err
	}
	dir, err := fieldString(fields, "dir")
	if err != nil {
		return TerminalSpec{}, err
	}
	columns, err := fieldInt(fields, "columns")
	if err != nil {
		return TerminalSpec{}, err
	}
	rows, err := fieldInt(fields, "rows")
	if err != nil {
		return TerminalSpec{}, err
	}
	env, err := optionalEnv(fields)
	if err != nil {
		return TerminalSpec{}, err
	}
	sandbox, err := optionalSandbox(fields)
	if err != nil {
		return TerminalSpec{}, err
	}
	return TerminalSpec{Dir: dir, Env: env, Sandbox: sandbox, Columns: int(columns), Rows: int(rows)}, nil
}

func encodeTerminalSpec(spec TerminalSpec) (jsontext.Value, error) {
	return encodeObject(func(enc *jsontext.Encoder) error {
		if err := writeString(enc, "dir", spec.Dir); err != nil {
			return err
		}
		if err := writeInt(enc, "columns", int64(spec.Columns)); err != nil {
			return err
		}
		if err := writeInt(enc, "rows", int64(spec.Rows)); err != nil {
			return err
		}
		if err := writeEnv(enc, spec.Env); err != nil {
			return err
		}
		return writeSandbox(enc, spec.Sandbox)
	})
}

func encodeTerminalID(id string) (jsontext.Value, error) {
	return encodeObject(func(enc *jsontext.Encoder) error { return writeString(enc, "terminalId", id) })
}

func encodeTerminalWritten(written int64, writeErr error) (jsontext.Value, error) {
	return encodeObject(func(enc *jsontext.Encoder) error {
		if err := writeInt(enc, "written", written); err != nil {
			return err
		}
		if writeErr == nil {
			return nil
		}
		return writeString(enc, "error", writeErr.Error())
	})
}

func encodeObject(members func(enc *jsontext.Encoder) error) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := members(enc); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}
