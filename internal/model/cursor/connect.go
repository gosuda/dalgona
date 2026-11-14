package cursor

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net/http"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"github.com/gosuda/dalgona/internal/model/cursor/wire"
)

const (
	maxFramePayload = 16 << 20
	maxQueuedFrames = 64
)

var errStreamCut = errors.New("cursor: stream ended before turnEnded")

func failureStatus(err error) int {
	if errors.Is(err, errStreamCut) {
		return http.StatusServiceUnavailable
	}
	if status := connectwire.ErrorStatus(err); status != 0 {
		return status
	}
	return http.StatusOK
}

type frameRead struct {
	frame connectwire.Frame
	err   error
}

type frameReader struct {
	body    *model.IdleBody
	current []byte
	spare   []byte
	queued  []frameRead
	ahead   chan frameRead
}

func newFrameReader(body *model.IdleBody) *frameReader {
	return &frameReader{body: body, current: make([]byte, 0, 32<<10)}
}

func (r *frameReader) next() (connectwire.Frame, error) {
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
	frame, err := connectwire.ReadFrame(r.body, r.current, maxFramePayload)
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
			frame, err := connectwire.ReadFrame(r.body, buffer, maxFramePayload)
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
	_ = r.body.Close()
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
	w.frame = connectwire.AppendFrame(w.frame[:0], 0, w.payload)
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
