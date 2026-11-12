package devin

import (
	"bytes"
	"compress/gzip"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"net/http"
	"sync/atomic"
	"time"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/connectwire"
)

const maxFramePayload = 16 << 20

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

func connectStatus(err error, fallback int) int {
	if connectErr, ok := errors.AsType[*connectwire.ConnectError](err); ok {
		if status, known := connectHTTPStatus[connectErr.Code]; known {
			return status
		}
	}
	return fallback
}

type inflater struct {
	reader *gzip.Reader
	out    bytes.Buffer
}

func (f *inflater) payload(frame connectwire.Frame) ([]byte, error) {
	if frame.Flags&connectwire.Compressed == 0 {
		return frame.Payload, nil
	}
	source := bytes.NewReader(frame.Payload)
	if f.reader == nil {
		reader, err := gzip.NewReader(source)
		if err != nil {
			return nil, fmt.Errorf("gunzip frame: %w", err)
		}
		f.reader = reader
	} else if err := f.reader.Reset(source); err != nil {
		return nil, fmt.Errorf("gunzip frame: %w", err)
	}
	f.out.Reset()
	written, err := f.out.ReadFrom(io.LimitReader(f.reader, maxFramePayload+1))
	if err != nil {
		return nil, fmt.Errorf("gunzip frame: %w", err)
	}
	if written > maxFramePayload {
		return nil, errors.New("gunzipped frame exceeds 16 MiB")
	}
	return f.out.Bytes(), nil
}

func gzipFrame(payload []byte) ([]byte, error) {
	var buffer bytes.Buffer
	buffer.Grow(len(payload)/2 + 64)
	buffer.Write([]byte{connectwire.Compressed, 0, 0, 0, 0})
	writer := gzip.NewWriter(&buffer)
	if _, err := writer.Write(payload); err != nil {
		return nil, fmt.Errorf("gzip request: %w", err)
	}
	if err := writer.Close(); err != nil {
		return nil, fmt.Errorf("gzip request: %w", err)
	}
	frame := buffer.Bytes()
	size := len(frame) - 5
	if size < 0 || size > maxFramePayload {
		return nil, errors.New("request frame exceeds 16 MiB")
	}
	binary.BigEndian.PutUint32(frame[1:5], uint32(size))
	return frame, nil
}

type idleBody struct {
	body    io.ReadCloser
	idle    time.Duration
	timer   *time.Timer
	expired atomic.Bool
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
	if n > 0 && w.timer != nil {
		w.timer.Reset(w.idle)
	}
	if err != nil && w.expired.Load() {
		return n, model.ErrStreamIdleTimeout
	}
	return n, err
}

func (w *idleBody) stop() {
	if w.timer != nil {
		w.timer.Stop()
	}
}
