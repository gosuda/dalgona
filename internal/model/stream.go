package model

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"io"
	"sync"
	"sync/atomic"
	"time"
)

type SSEHandler func(event, data []byte) error

const (
	maxSSELineBytes  = 1 << 20
	maxSSEEventBytes = 8 << 20
)

var (
	ErrStreamStartTimeout = errors.New("model: first stream event timeout")
	ErrStreamIdleTimeout  = errors.New("model: stream idle timeout")
	ErrSSELineTooLarge    = errors.New("model: sse line too large")
	ErrSSEEventTooLarge   = errors.New("model: sse event too large")
)

func (p *sseParser) appendData(value []byte) error {
	add := len(value)
	if len(p.data) != 0 {
		add++
	}
	if len(p.data)+add > maxSSEEventBytes {
		return ErrSSEEventTooLarge
	}
	if len(p.data) != 0 {
		p.data = append(p.data, '\n')
	}
	p.data = append(p.data, value...)
	return nil
}

func (p *sseParser) consume(line []byte) error {
	line = bytes.TrimSuffix(line, []byte{'\n'})
	line = bytes.TrimSuffix(line, []byte{'\r'})
	switch {
	case len(line) == 0:
		return p.flush()
	case bytes.HasPrefix(line, []byte("event:")):
		p.event = append(p.event[:0], bytes.TrimSpace(line[6:])...)
	case bytes.HasPrefix(line, []byte("data:")):
		return p.appendData(bytes.TrimSpace(line[5:]))
	case line[0] == '{' || line[0] == '[':
		p.data = append(p.data[:0], line...)
	}
	return nil
}

func readSSELine(reader *bufio.Reader) ([]byte, error) {
	var line []byte
	for {
		part, err := reader.ReadSlice('\n')
		line = append(line, part...)
		if len(line) > maxSSELineBytes {
			return nil, ErrSSELineTooLarge
		}
		if !errors.Is(err, bufio.ErrBufferFull) {
			return line, err
		}
	}
}

type StartGate struct {
	cancel context.CancelCauseFunc
	timer  *time.Timer
	once   sync.Once
}

func NewStartGate(ctx context.Context, timeout time.Duration) (context.Context, *StartGate) {
	gateCtx, cancel := context.WithCancelCause(ctx)
	gate := &StartGate{cancel: cancel}
	if timeout > 0 {
		gate.timer = time.AfterFunc(timeout, func() { cancel(ErrStreamStartTimeout) })
	}
	return gateCtx, gate
}

func (g *StartGate) FirstEvent() {
	g.once.Do(func() {
		if g.timer != nil {
			g.timer.Stop()
		}
	})
}

func (g *StartGate) Close() {
	if g.timer != nil {
		g.timer.Stop()
	}
	g.cancel(nil)
}

func (g *StartGate) Error(ctx context.Context, err error) error {
	if err == nil {
		return nil
	}
	if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		return context.Cause(ctx)
	}
	return err
}

type sseParser struct {
	event   []byte
	data    []byte
	handler SSEHandler
}

func ReadSSE(ctx context.Context, body io.Reader, handler SSEHandler) error {
	reader := bufio.NewReaderSize(body, 32*1024)
	parser := sseParser{handler: handler}
	for {
		line, err := readSSELine(reader)
		if len(line) != 0 {
			if consumeErr := parser.consume(line); consumeErr != nil {
				return consumeErr
			}
		}
		if err != nil {
			return parser.finish(err)
		}
		if ctxErr := ctx.Err(); ctxErr != nil {
			return ctxErr
		}
	}
}

func (p *sseParser) flush() error {
	if len(p.event) == 0 && len(p.data) == 0 {
		return nil
	}
	if err := p.handler(p.event, p.data); err != nil {
		return err
	}
	p.event, p.data = p.event[:0], p.data[:0]
	return nil
}

func (p *sseParser) finish(err error) error {
	if errors.Is(err, io.EOF) {
		return p.flush()
	}
	return err
}

type StreamTimers struct {
	paused atomic.Bool
	change chan struct{}
}

func NewStreamTimers() *StreamTimers {
	return &StreamTimers{change: make(chan struct{}, 1)}
}

func (t *StreamTimers) PauseIdle() {
	t.paused.Store(true)
	t.notify()
}

func (t *StreamTimers) ResumeIdle() {
	t.paused.Store(false)
	t.notify()
}

func (t *StreamTimers) notify() {
	select {
	case t.change <- struct{}{}:
	default:
	}
}

type SSEOptions struct {
	StartTimeout time.Duration
	IdleTimeout  time.Duration
	Control      *StreamTimers
}

type activityReader struct {
	reader   io.Reader
	activity chan<- struct{}
}

func (r activityReader) Read(p []byte) (int, error) {
	n, err := r.reader.Read(p)
	if n != 0 {
		select {
		case r.activity <- struct{}{}:
		default:
		}
	}
	return n, err
}

type sseDeadlines struct {
	start        *time.Timer
	idle         *time.Timer
	startC       <-chan time.Time
	idleC        <-chan time.Time
	idleDuration time.Duration
}

func newSSEDeadlines(options SSEOptions) *sseDeadlines {
	d := &sseDeadlines{}
	if options.StartTimeout > 0 {
		d.start = time.NewTimer(options.StartTimeout)
		d.startC = d.start.C
	}
	if options.IdleTimeout > 0 {
		d.idle = time.NewTimer(options.IdleTimeout)
		d.idleC = d.idle.C
		d.idleDuration = options.IdleTimeout
	}
	return d
}

func (d *sseDeadlines) firstEvent() {
	stopTimer(d.start)
	d.startC = nil
}

func (d *sseDeadlines) activity(control *StreamTimers) {
	if control.paused.Load() {
		stopTimer(d.idle)
		d.idleC = nil
		return
	}
	resetTimer(d.idle, d.idleDuration)
	if d.idle != nil {
		d.idleC = d.idle.C
	}
}

func (d *sseDeadlines) stop() {
	stopTimer(d.start)
	stopTimer(d.idle)
}

func stopTimer(timer *time.Timer) {
	if timer == nil {
		return
	}
	if !timer.Stop() {
		select {
		case <-timer.C:
		default:
		}
	}
}

func resetTimer(timer *time.Timer, duration time.Duration) {
	if timer == nil {
		return
	}
	stopTimer(timer)
	timer.Reset(duration)
}

type sseSignals struct {
	activity chan struct{}
	first    chan struct{}
	result   chan error
}

func ReadSSEWithTimeouts(ctx context.Context, body io.ReadCloser, options SSEOptions, handler SSEHandler) error {
	control := options.Control
	if control == nil {
		control = NewStreamTimers()
	}
	signals := &sseSignals{
		activity: make(chan struct{}, 1),
		first:    make(chan struct{}, 1),
		result:   make(chan error, 1),
	}
	readCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	go readTimedSSE(readCtx, body, signals, handler)
	return waitTimedSSE(ctx, body, control, signals, options)
}

func readTimedSSE(ctx context.Context, body io.ReadCloser, signals *sseSignals, handler SSEHandler) {
	notifyFirst := sync.OnceFunc(func() { signals.first <- struct{}{} })
	reader := activityReader{reader: body, activity: signals.activity}
	signals.result <- ReadSSE(ctx, reader, func(event, data []byte) error {
		notifyFirst()
		return handler(event, data)
	})
}

func waitTimedSSE(ctx context.Context, body io.Closer, control *StreamTimers, signals *sseSignals, options SSEOptions) error {
	deadlines := newSSEDeadlines(options)
	defer deadlines.stop()
	for {
		select {
		case err := <-signals.result:
			return err
		case <-signals.first:
			deadlines.firstEvent()
		case <-signals.activity:
			deadlines.activity(control)
		case <-control.change:
			deadlines.activity(control)
		case <-deadlines.startC:
			return abandonSSE(body, signals, ErrStreamStartTimeout)
		case <-deadlines.idleC:
			return abandonSSE(body, signals, ErrStreamIdleTimeout)
		case <-ctx.Done():
			return abandonSSE(body, signals, ctx.Err())
		}
	}
}

func abandonSSE(body io.Closer, signals *sseSignals, cause error) error {
	_ = body.Close()
	if readErr := <-signals.result; readErr == nil {
		return nil
	}
	return cause
}

func (g *StartGate) ConsumeSSE(ctx context.Context, body io.ReadCloser, timers *StreamTimers, idleTimeout time.Duration, handle SSEHandler) error {
	err := ReadSSEWithTimeouts(ctx, body, SSEOptions{IdleTimeout: idleTimeout, Control: timers}, func(event, data []byte) error {
		g.FirstEvent()
		return handle(event, data)
	})
	return g.Error(ctx, err)
}

func LaunchStream(ctx context.Context, retry RetryOptions, attempt func(context.Context, *EventStream) AttemptResult) (*EventStream, error) {
	stream := NewEventStream(ctx, 64)
	go func() {
		streamCtx := stream.Context(ctx)
		result := RunRetries(streamCtx, retry, func(attemptCtx context.Context) AttemptResult {
			return attempt(attemptCtx, stream)
		})
		if result.Err != nil {
			stream.Emit(Error{Err: result.Err, Message: result.Err.Error(), Status: result.Status, Body: result.Body})
		}
		stream.Finish(nil)
	}()
	return stream, nil
}

type IdleBody struct {
	body    io.ReadCloser
	idle    time.Duration
	timer   *time.Timer
	expired atomic.Bool
	paused  atomic.Bool
}

func WatchIdle(body io.ReadCloser, idle time.Duration) *IdleBody {
	watched := &IdleBody{body: body, idle: idle}
	if idle > 0 {
		watched.timer = time.AfterFunc(idle, watched.expire)
	}
	return watched
}

func (w *IdleBody) expire() {
	w.expired.Store(true)
	_ = w.body.Close()
}

func (w *IdleBody) Read(p []byte) (int, error) {
	n, err := w.body.Read(p)
	if n > 0 && w.timer != nil && !w.paused.Load() {
		w.timer.Reset(w.idle)
	}
	if err != nil && w.expired.Load() {
		return n, ErrStreamIdleTimeout
	}
	return n, err
}

func (w *IdleBody) Pause() {
	w.paused.Store(true)
	if w.timer != nil {
		w.timer.Stop()
	}
}

func (w *IdleBody) Resume() {
	w.paused.Store(false)
	if w.timer != nil {
		w.timer.Reset(w.idle)
	}
}

func (w *IdleBody) Stop() {
	if w.timer != nil {
		w.timer.Stop()
	}
}

func (w *IdleBody) Close() error {
	w.Stop()
	return w.body.Close()
}
