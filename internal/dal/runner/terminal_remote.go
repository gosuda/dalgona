package runner

import (
	"context"
	"encoding/base64"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"sync"
	"time"
)

const (
	remoteTerminalCall     = 10 * time.Second
	remoteTerminalPollMin  = 5 * time.Millisecond
	remoteTerminalPollIdle = 200 * time.Millisecond
	remoteTerminalRetryMax = time.Second
	remoteTerminalStall    = 30 * time.Second
)

type remoteTerminal struct {
	remote  *Remote
	id      string
	mu      sync.Mutex
	offset  int64
	pending []byte
	ended   bool
	exit    Exit
	done    chan struct{}
	exited  sync.Once
	closed  chan struct{}
	close   sync.Once
	wake    chan struct{}
	failing time.Time
}

func (r *Remote) OpenTerminal(ctx context.Context, spec TerminalSpec) (Terminal, error) {
	if spec.Columns < 1 || spec.Rows < 1 {
		return nil, errTerminalSize
	}
	spec.Sandbox = Sandbox{Mode: SandboxOff}
	params, err := encodeTerminalSpec(spec)
	if err != nil {
		return nil, err
	}
	result, err := r.call(ctx, "terminal/open", params)
	if err != nil {
		return nil, fmt.Errorf("runner: open remote terminal: %w", err)
	}
	fields, err := objectFields(result)
	if err != nil {
		return nil, err
	}
	id, err := fieldString(fields, "terminalId")
	if err != nil {
		return nil, err
	}
	return &remoteTerminal{remote: r, id: id, done: make(chan struct{}), closed: make(chan struct{}), wake: make(chan struct{}, 1)}, nil
}

func (t *remoteTerminal) call(method string, members func(enc *jsontext.Encoder) error) (jsontext.Value, error) {
	return t.callTimeout(method, members, remoteTerminalCall)
}

func (t *remoteTerminal) callTimeout(method string, members func(enc *jsontext.Encoder) error, timeout time.Duration) (jsontext.Value, error) {
	params, err := encodeObject(func(enc *jsontext.Encoder) error {
		if err := writeString(enc, "terminalId", t.id); err != nil {
			return err
		}
		return members(enc)
	})
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	return t.remote.call(ctx, method, params)
}

func (t *remoteTerminal) Read(p []byte) (int, error) {
	wait := remoteTerminalPollMin
	for {
		if n, ok, err := t.take(p); ok {
			return n, err
		}
		got, err := t.fetch()
		if err != nil {
			if lost := t.stalled(err); lost != nil {
				return 0, lost
			}
		}
		if got {
			wait = remoteTerminalPollMin
			continue
		}
		limit := remoteTerminalPollIdle
		if err != nil {
			limit = remoteTerminalRetryMax
		}
		if wait, err = t.park(wait, limit); err != nil {
			return 0, err
		}
	}
}

func (t *remoteTerminal) park(wait, limit time.Duration) (time.Duration, error) {
	select {
	case <-t.closed:
		return 0, io.EOF
	case <-t.wake:
		return remoteTerminalPollMin, nil
	case <-time.After(wait):
		return min(wait*2, limit), nil
	}
}

func (t *remoteTerminal) stalled(err error) error {
	if t.failing.IsZero() {
		t.failing = time.Now()
	}
	if errors.Is(err, ErrProcessNotFound) || errors.Is(err, errRemoteClosed) || t.leaseExceeded() {
		t.finish(Exit{Code: -1, EndedAt: time.Now()})
		return fmt.Errorf("%w: %w", ErrTerminalLost, err)
	}
	return nil
}

func (t *remoteTerminal) leaseExceeded() bool {
	return !t.failing.IsZero() && time.Since(t.failing) >= remoteTerminalStall
}

func (t *remoteTerminal) take(p []byte) (int, bool, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if len(t.pending) > 0 {
		n := copy(p, t.pending)
		t.pending = t.pending[n:]
		return n, true, nil
	}
	if t.ended {
		return 0, true, io.EOF
	}
	return 0, false, nil
}

func (t *remoteTerminal) fetch() (bool, error) {
	t.mu.Lock()
	offset := t.offset
	t.mu.Unlock()
	timeout := remoteTerminalCall
	if !t.failing.IsZero() {
		remaining := remoteTerminalStall - time.Since(t.failing)
		if remaining <= 0 {
			return false, context.DeadlineExceeded
		}
		timeout = min(timeout, remaining)
	}
	result, err := t.callTimeout("terminal/read", func(enc *jsontext.Encoder) error { return writeInt(enc, "offset", offset) }, timeout)
	if err != nil {
		return false, err
	}
	out, err := decodeOutput(result)
	if err != nil {
		return false, err
	}
	if t.leaseExceeded() {
		return false, context.DeadlineExceeded
	}
	t.failing = time.Time{}
	if out.Exit != nil {
		t.finish(*out.Exit)
	}
	t.mu.Lock()
	defer t.mu.Unlock()
	t.offset = out.Next
	t.pending = append(t.pending, out.Data...)
	t.ended = !out.Alive && out.Exit != nil
	return len(out.Data) > 0 || t.ended, nil
}

func (t *remoteTerminal) finish(exit Exit) {
	t.exited.Do(func() {
		t.exit = exit
		close(t.done)
	})
}

func (t *remoteTerminal) Write(p []byte) (int, error) {
	result, err := t.call("terminal/write", func(enc *jsontext.Encoder) error {
		return writeString(enc, "dataBase64", base64.StdEncoding.EncodeToString(p))
	})
	if err != nil {
		return 0, err
	}
	select {
	case t.wake <- struct{}{}:
	default:
	}
	fields, err := objectFields(result)
	if err != nil {
		return 0, err
	}
	written, err := fieldInt(fields, "written")
	if err != nil {
		return 0, err
	}
	failure, err := optionalString(fields, "error")
	if err != nil {
		return int(written), err
	}
	if failure != "" || int(written) != len(p) {
		return int(written), fmt.Errorf("runner: remote terminal write: %s", failure)
	}
	return len(p), nil
}

func (t *remoteTerminal) Resize(columns, rows int) error {
	_, err := t.call("terminal/resize", func(enc *jsontext.Encoder) error {
		if err := writeInt(enc, "columns", int64(columns)); err != nil {
			return err
		}
		return writeInt(enc, "rows", int64(rows))
	})
	return err
}

func (t *remoteTerminal) Signal(signal Signal) error {
	name, err := signalName(signal)
	if err != nil {
		return err
	}
	_, err = t.call("terminal/signal", func(enc *jsontext.Encoder) error { return writeString(enc, "signal", name) })
	return err
}

func (t *remoteTerminal) Done() <-chan struct{} { return t.done }

func (t *remoteTerminal) Exit() Exit {
	<-t.done
	return t.exit
}

func (t *remoteTerminal) Close() error {
	var err error
	t.close.Do(func() {
		close(t.closed)
		_, err = t.call("terminal/close", func(*jsontext.Encoder) error { return nil })
	})
	return err
}
