package cursor

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net/http"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/cursor/wire"
)

const (
	maxFramePayload = 16 << 20
	maxQueuedFrames = 64
)

var errStreamCut = errors.New("cursor: stream ended before turnEnded")

// source: https://connectrpc.com/docs/protocol/#error-codes
var connectHTTPStatus = map[string]int{
	"canceled":            499,
	"unknown":             http.StatusInternalServerError,
	"invalid_argument":    http.StatusBadRequest,
	"deadline_exceeded":   http.StatusGatewayTimeout,
	"not_found":           http.StatusNotFound,
	"already_exists":      http.StatusConflict,
	"permission_denied":   http.StatusForbidden,
	"resource_exhausted":  http.StatusTooManyRequests,
	"failed_precondition": http.StatusBadRequest,
	"aborted":             http.StatusConflict,
	"out_of_range":        http.StatusBadRequest,
	"unimplemented":       http.StatusNotImplemented,
	"internal":            http.StatusInternalServerError,
	"unavailable":         http.StatusServiceUnavailable,
	"data_loss":           http.StatusInternalServerError,
	"unauthenticated":     http.StatusUnauthorized,
}

func failureStatus(err error) int {
	if errors.Is(err, errStreamCut) {
		return http.StatusServiceUnavailable
	}
	if connectErr, ok := errors.AsType[*wire.ConnectError](err); ok {
		if status, known := connectHTTPStatus[connectErr.Code]; known {
			return status
		}
	}
	return http.StatusOK
}

type idleBody struct {
	body    io.ReadCloser
	idle    time.Duration
	timer   *time.Timer
	expired atomic.Bool
	paused  atomic.Bool
}

func watchIdle(body io.ReadCloser, idle time.Duration) *idleBody {
	watched := &idleBody{body: body, idle: idle}
	if idle > 0 {
		watched.timer = time.AfterFunc(idle, watched.expire)
	}
	return watched
}

func (w *idleBody) expire() {
	w.expired.Store(true)
	_ = w.body.Close()
}

func (w *idleBody) Read(p []byte) (int, error) {
	n, err := w.body.Read(p)
	if n > 0 && w.timer != nil && !w.paused.Load() {
		w.timer.Reset(w.idle)
	}
	if err != nil && w.expired.Load() {
		return n, model.ErrStreamIdleTimeout
	}
	return n, err
}

func (w *idleBody) pause() {
	w.paused.Store(true)
	if w.timer != nil {
		w.timer.Stop()
	}
}

func (w *idleBody) resume() {
	w.paused.Store(false)
	if w.timer != nil {
		w.timer.Reset(w.idle)
	}
}

type frameRead struct {
	frame wire.Frame
	err   error
}

type frameReader struct {
	body    *idleBody
	current []byte
	spare   []byte
	queued  []frameRead
	ahead   chan frameRead
}

func newFrameReader(body *idleBody) *frameReader {
	return &frameReader{body: body, current: make([]byte, 0, 32<<10)}
}

func (r *frameReader) next() (wire.Frame, error) {
	if len(r.queued) != 0 {
		read := r.queued[0]
		r.queued = r.queued[1:]
		return read.frame, read.err
	}
	if r.ahead != nil {
		read := <-r.ahead
		r.ahead = nil
		r.spare, r.current = r.current, read.frame.Payload
		return read.frame, read.err
	}
	frame, err := wire.ReadFrame(r.body, r.current, maxFramePayload)
	if err == nil {
		r.current = frame.Payload[:0]
	}
	return frame, err
}

func (r *frameReader) lookahead() <-chan frameRead {
	if r.ahead == nil {
		ahead := make(chan frameRead, 1)
		r.ahead = ahead
		go func(buffer []byte) {
			frame, err := wire.ReadFrame(r.body, buffer, maxFramePayload)
			ahead <- frameRead{frame: frame, err: err}
		}(r.spare)
	}
	return r.ahead
}

func (r *frameReader) landed(read frameRead) {
	r.ahead = nil
	if read.err == nil {
		r.spare = read.frame.Payload[:0]
	}
}

func (r *frameReader) queue(read frameRead) bool {
	read.frame.Payload = bytes.Clone(read.frame.Payload)
	r.queued = append(r.queued, read)
	return len(r.queued) >= maxQueuedFrames
}

func (r *frameReader) stop() {
	if r.ahead == nil {
		return
	}
	_ = r.body.body.Close()
	<-r.ahead
	r.ahead = nil
}

type frameWriter struct {
	mu      sync.Mutex
	pipe    *io.PipeWriter
	payload []byte
	frame   []byte
}

func (w *frameWriter) send(message *wire.AgentClientMessage) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.payload = wire.AppendAgentClientMessage(w.payload[:0], message)
	w.frame = wire.AppendFrame(w.frame[:0], 0, w.payload)
	_, err := w.pipe.Write(w.frame)
	return err
}

func (w *frameWriter) close() {
	_ = w.pipe.Close()
}

func (w *frameWriter) execControl(kind int32, id uint32, failure string) error {
	control := wire.AppendExecClientControl(nil, &wire.ExecClientControl{Kind: kind, ID: id, ErrorView: []byte(failure)})
	return w.send(&wire.AgentClientMessage{MessageNumber: wire.ClientExecControlNumber, MessageView: control})
}

func repeat(ctx context.Context, period time.Duration, beat func() error) func() {
	ctx, cancel := context.WithCancel(ctx)
	done := make(chan struct{})
	go func() {
		defer close(done)
		ticker := time.NewTicker(period)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				if beat() != nil {
					return
				}
			}
		}
	}()
	return func() {
		cancel()
		<-done
	}
}
