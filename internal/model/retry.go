package model

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"math/rand/v2"
	"net"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"
)

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
		if status == http.StatusTooManyRequests && isQuota(body) {
			return Terminal
		}
		return verdict
	}
	if status != 0 && status != http.StatusOK {
		return Terminal
	}
	if hasRetryableCode(body) && !isQuota(body) {
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

var quotaCodes = [...]string{"insufficient_quota", "credit_balance_exhausted", "organization_spend_limit_exceeded", "project_spend_limit_exceeded", "organization_usage_limit_exceeded", "usage_limit_reached", "usage_not_included"}

func isQuota(body []byte) bool {
	lower := strings.ToLower(string(body))
	for _, code := range quotaCodes {
		if strings.Contains(lower, `"`+code+`"`) {
			return true
		}
	}
	return false
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

func HTTPFailure(provider string, resp *http.Response) AttemptResult {
	body, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return AttemptResult{Status: resp.StatusCode, Err: fmt.Errorf("%s: read error response: %w", provider, err)}
	}
	return AttemptResult{
		Status:     resp.StatusCode,
		RetryAfter: RetryAfter(resp.Header.Get("Retry-After")),
		Body:       body,
		Err:        fmt.Errorf("%s: status %d: %s", provider, resp.StatusCode, bytes.TrimSpace(body)),
	}
}
