package model

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"io"
	"math/rand/v2"
	"net"
	"net/http"
	"os"
	"strconv"
	"strings"
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

type Verdict uint8

const (
	Retry Verdict = iota + 1
	Overflow
	Auth
	Terminal
)

func Classify(status int, body []byte) Verdict {
	if isOverflow(body) {
		return Overflow
	}
	if verdict := classifyHTTPStatus(status); verdict != 0 {
		return verdict
	}
	if status != 0 && status != http.StatusOK {
		return Terminal
	}
	if hasRetryableCode(body) {
		return Retry
	}
	return Terminal
}

func classifyHTTPStatus(status int) Verdict {
	switch {
	case status == http.StatusUnauthorized || status == http.StatusForbidden:
		return Auth
	case status == 0 || status == http.StatusRequestTimeout || status == http.StatusTooManyRequests || status >= 500:
		return Retry
	case status >= 400:
		return Terminal
	default:
		return 0
	}
}

func isOverflow(body []byte) bool {
	lower := strings.ToLower(string(body))
	return strings.Contains(lower, "context_length_exceeded") ||
		strings.Contains(lower, "prompt is too long") ||
		strings.Contains(lower, "exceeds the maximum number of tokens")
}

func hasRetryableCode(body []byte) bool {
	compact := strings.ReplaceAll(strings.ToLower(string(body)), " ", "")
	if containsStatusCode(compact, 429) {
		return true
	}
	for code := 500; code <= 599; code++ {
		if containsStatusCode(compact, code) {
			return true
		}
	}
	return false
}

func containsStatusCode(body string, code int) bool {
	number := strconv.Itoa(code)
	for _, key := range []string{`"code"`, `"status"`, "code", "status"} {
		for _, separator := range []string{":", "="} {
			if strings.Contains(body, key+separator+number) || strings.Contains(body, key+separator+`"`+number+`"`) {
				return true
			}
		}
	}
	return false
}

func hasTransientText(body []byte) bool {
	lower := strings.ToLower(string(body))
	phrases := [...]string{
		"context deadline exceeded", "i/o timeout", "timeout", "unexpected eof",
		"connection reset", "connection refused", "broken pipe", "tls: bad record mac", "stream ended",
	}
	for _, phrase := range phrases {
		if strings.Contains(lower, phrase) {
			return true
		}
	}
	return false
}

type RetryOptions struct {
	StartTimeout time.Duration
	IdleTimeout  time.Duration
	Sleep        func(context.Context, time.Duration) error
	OnRetry      func(attempt int, verdict Verdict, status int, delay time.Duration)
	Random       func() float64
}

type AttemptResult struct {
	Status     int
	RetryAfter time.Duration
	SawTool    bool
	SawContent bool
	Body       []byte
	Err        error
}

type retryWatchKey struct{}

type RetryWatch struct {
	Attended func() bool
	Tick     func(remaining time.Duration)
}

func WithRetryWatch(ctx context.Context, watch RetryWatch) context.Context {
	return context.WithValue(ctx, retryWatchKey{}, watch)
}

func retryWatchFrom(ctx context.Context) RetryWatch {
	watch, _ := ctx.Value(retryWatchKey{}).(RetryWatch)
	return watch
}

func (w RetryWatch) attended() bool {
	return w.Attended != nil && w.Attended()
}

func DefaultRetryOptions() RetryOptions {
	return RetryOptions{StartTimeout: time.Minute, IdleTimeout: 5 * time.Minute, Sleep: sleepContext, Random: rand.Float64}
}

func RunRetries(ctx context.Context, options RetryOptions, attempt func(context.Context) AttemptResult) AttemptResult {
	for index := range 4 {
		result := attempt(ctx)
		if result.Err == nil {
			return result
		}
		if ctx.Err() != nil {
			result.Err = ctx.Err()
			return result
		}
		verdict := classifyAttempt(result)
		if !retryableAttempt(result, verdict, index) {
			return result
		}
		if err := waitError(ctx, result.RetryAfter); err != nil {
			result.Err = err
			return result
		}
		delay := RetryDelay(index, result.RetryAfter, options.Random)
		if options.OnRetry != nil {
			options.OnRetry(index+1, verdict, result.Status, delay)
		}
		if err := retrySleep(ctx, options.Sleep, delay); err != nil {
			result.Err = err
			return result
		}
	}
	return AttemptResult{Err: errors.New("model: retry loop exhausted")}
}

func retryableAttempt(result AttemptResult, verdict Verdict, index int) bool {
	if result.SawTool || result.SawContent || verdict != Retry {
		return false
	}
	return index < 3
}

func classifyAttempt(result AttemptResult) Verdict {
	if len(result.Body) != 0 {
		return Classify(result.Status, result.Body)
	}
	message := []byte(result.Err.Error())
	if result.Status == 0 {
		if isOverflow(message) {
			return Overflow
		}
		if transportFailure(result.Err) || transportTextFailure(result.Err, message) {
			return Retry
		}
		return Terminal
	}
	verdict := Classify(result.Status, message)
	if verdict == Terminal && transientError(result.Err, message) {
		return Retry
	}
	return verdict
}

func transportFailure(err error) bool {
	if errors.Is(err, ErrStreamStartTimeout) || errors.Is(err, ErrStreamIdleTimeout) {
		return true
	}
	_, isTransport := errors.AsType[net.Error](err)
	return isTransport
}

func transportTextFailure(err error, message []byte) bool {
	if !hasTransientText(message) {
		return false
	}
	_, isSyscall := errors.AsType[*os.SyscallError](err)
	return errors.Is(err, io.ErrUnexpectedEOF) ||
		errors.Is(err, io.ErrClosedPipe) ||
		isSyscall
}

func transientError(err error, message []byte) bool {
	return errors.Is(err, ErrStreamStartTimeout) || errors.Is(err, ErrStreamIdleTimeout) || hasTransientText(message)
}

func retrySleep(ctx context.Context, sleep func(context.Context, time.Duration) error, delay time.Duration) error {
	if sleep == nil {
		sleep = sleepContext
	}
	watch := retryWatchFrom(ctx)
	ticking := watch.Tick != nil && delay > maxUnattendedRetryAfter
	if ticking {
		defer watch.Tick(0)
	}
	for remaining := delay; remaining > 0; {
		if err := ctx.Err(); err != nil {
			return err
		}
		if ticking {
			watch.Tick(remaining)
		}
		step := min(remaining, time.Second)
		if err := sleep(ctx, step); err != nil {
			return err
		}
		remaining -= step
	}
	return nil
}

func sleepContext(ctx context.Context, delay time.Duration) error {
	timer := time.NewTimer(delay)
	defer timer.Stop()
	select {
	case <-timer.C:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

const (
	maxRetryAfter           = 5 * time.Minute
	maxUnattendedRetryAfter = time.Minute
)

func RetryDelay(attempt int, retryAfter time.Duration, random func() float64) time.Duration {
	if retryAfter > 0 {
		return retryAfter
	}
	base := time.Second << min(attempt, 2)
	if random == nil {
		random = rand.Float64
	}
	factor := 0.8 + random()*0.4
	return time.Duration(float64(base) * factor)
}

func waitError(ctx context.Context, wait time.Duration) error {
	limit := maxUnattendedRetryAfter
	if wait > limit && retryWatchFrom(ctx).attended() {
		limit = maxRetryAfter
	}
	if wait <= limit {
		return nil
	}
	seconds := int64((wait + time.Second - 1) / time.Second)
	return errors.New("rate limited for " + strconv.FormatInt(seconds, 10) + " s, over the " + strconv.FormatInt(int64(limit/time.Second), 10) + " s wait limit")
}

func RetryAfter(header string) time.Duration {
	if header == "" {
		return 0
	}
	seconds, err := strconv.Atoi(strings.TrimSpace(header))
	if err == nil {
		if seconds < 0 {
			return 0
		}
		return time.Duration(seconds) * time.Second
	}
	date, err := http.ParseTime(header)
	if err != nil {
		return 0
	}
	return max(time.Until(date), 0)
}
