package model

import (
	"context"
	"errors"
	"net"
	"net/http"
	"net/url"
	"testing"
	"time"
)

func TestClassifyAttemptRetriesOnlyTransportAtStatusZero(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name   string
		result AttemptResult
		want   Verdict
	}{
		{name: "encode failure", result: AttemptResult{Err: errors.New("openai: encode request: bad tool schema")}, want: Terminal},
		{name: "provider status in text", result: AttemptResult{Err: errors.New("antigravity: /v1internal:loadCodeAssist: http 403: denied")}, want: Terminal},
		{name: "provider text with timeout", result: AttemptResult{Err: errors.New("provider: upstream timeout, try later")}, want: Terminal},
		{name: "url error", result: AttemptResult{Err: &url.Error{Op: "Post", URL: "https://example.com/v1", Err: errors.New("dial tcp: connection refused")}}, want: Retry},
		{name: "net error", result: AttemptResult{Err: &net.OpError{Op: "read", Err: errors.New("connection reset by peer")}}, want: Retry},
		{name: "start timeout", result: AttemptResult{Err: ErrStreamStartTimeout}, want: Retry},
		{name: "idle timeout", result: AttemptResult{Err: ErrStreamIdleTimeout}, want: Retry},
		{name: "overflow in message", result: AttemptResult{Err: errors.New("google: context_length_exceeded")}, want: Overflow},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			if got := classifyAttempt(test.result); got != test.want {
				t.Fatalf("classifyAttempt(%+v) = %v, want %v", test.result, got, test.want)
			}
		})
	}
}

func TestRunRetriesStopsOnDeterministicStatusZero(t *testing.T) {
	t.Parallel()
	var attempts int
	result := RunRetries(t.Context(), RetryOptions{Sleep: func(context.Context, time.Duration) error { return nil }}, func(context.Context) AttemptResult {
		attempts++
		return AttemptResult{Err: errors.New("chatcompat: encode request: boom")}
	})
	if attempts != 1 || result.Err == nil {
		t.Fatalf("attempts = %d, err = %v; want 1 attempt and an error", attempts, result.Err)
	}
}

func TestRunRetriesRepeatsTransportFailure(t *testing.T) {
	t.Parallel()
	var attempts int
	result := RunRetries(t.Context(), RetryOptions{Sleep: func(context.Context, time.Duration) error { return nil }}, func(context.Context) AttemptResult {
		attempts++
		return AttemptResult{Err: &url.Error{Op: "Post", URL: "https://example.com/v1", Err: errors.New("connection reset by peer")}}
	})
	if attempts != 4 || result.Err == nil {
		t.Fatalf("attempts = %d, err = %v; want 4 attempts and an error", attempts, result.Err)
	}
}

func TestRunRetriesStopsAfterContentEmitted(t *testing.T) {
	t.Parallel()
	var attempts int
	result := RunRetries(t.Context(), RetryOptions{Sleep: func(context.Context, time.Duration) error { return nil }}, func(context.Context) AttemptResult {
		attempts++
		return AttemptResult{SawContent: true, Err: &url.Error{Op: "Post", URL: "https://example.com/v1", Err: errors.New("connection reset by peer")}}
	})
	if attempts != 1 || result.Err == nil {
		t.Fatalf("attempts = %d, err = %v; want 1 attempt and an error", attempts, result.Err)
	}
}

var rateLimitBody = []byte(`{"error":{"type":"rate_limit_error"}}`)

func TestRunRetriesStopsOverTheRetryAfterLimit(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name     string
		wait     time.Duration
		attended bool
		want     string
	}{
		{name: "unattended 90 s", wait: 90 * time.Second, want: "rate limited for 90 s, over the 60 s wait limit"},
		{name: "unattended rounds up", wait: 61500 * time.Millisecond, want: "rate limited for 62 s, over the 60 s wait limit"},
		{name: "attended over 300 s", wait: 301 * time.Second, attended: true, want: "rate limited for 301 s, over the 300 s wait limit"},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			var attempts, ticks, sleeps int
			ctx := WithRetryWatch(t.Context(), RetryWatch{
				Attended: func() bool { return test.attended },
				Tick:     func(time.Duration) { ticks++ },
			})
			result := RunRetries(ctx, RetryOptions{Sleep: func(context.Context, time.Duration) error { sleeps++; return nil }}, func(context.Context) AttemptResult {
				attempts++
				return AttemptResult{Status: http.StatusTooManyRequests, RetryAfter: test.wait, Body: rateLimitBody, Err: errors.New("openai: http 429")}
			})
			if attempts != 1 || sleeps != 0 || ticks != 0 {
				t.Fatalf("attempts = %d, sleeps = %d, ticks = %d; want 1, 0, 0", attempts, sleeps, ticks)
			}
			if result.Err == nil || result.Err.Error() != test.want {
				t.Fatalf("err = %v, want %q", result.Err, test.want)
			}
			if result.Status != http.StatusTooManyRequests || string(result.Body) != string(rateLimitBody) {
				t.Fatalf("status = %d, body = %s; want 429 and the provider body", result.Status, result.Body)
			}
		})
	}
}

func TestRunRetriesWaitsAtTheRetryAfterLimit(t *testing.T) {
	t.Parallel()
	var attempts, ticks int
	var slept time.Duration
	ctx := WithRetryWatch(t.Context(), RetryWatch{Tick: func(time.Duration) { ticks++ }})
	result := RunRetries(ctx, RetryOptions{Sleep: func(_ context.Context, d time.Duration) error { slept += d; return nil }}, func(context.Context) AttemptResult {
		attempts++
		if attempts == 1 {
			return AttemptResult{Status: http.StatusTooManyRequests, RetryAfter: time.Minute, Body: rateLimitBody, Err: errors.New("openai: http 429")}
		}
		return AttemptResult{}
	})
	if result.Err != nil || attempts != 2 {
		t.Fatalf("err = %v, attempts = %d; want success on the second attempt", result.Err, attempts)
	}
	if slept != time.Minute || ticks != 0 {
		t.Fatalf("slept = %v, ticks = %d; want 1m0s and no tick without a client", slept, ticks)
	}
}

func TestRunRetriesWaitsWhileAttended(t *testing.T) {
	t.Parallel()
	var attempts int
	var slept time.Duration
	var ticked []time.Duration
	ctx := WithRetryWatch(t.Context(), RetryWatch{
		Attended: func() bool { return true },
		Tick:     func(remaining time.Duration) { ticked = append(ticked, remaining) },
	})
	result := RunRetries(ctx, RetryOptions{Sleep: func(_ context.Context, d time.Duration) error {
		if d > time.Second {
			t.Errorf("sleep step %v is above one second", d)
		}
		slept += d
		return nil
	}}, func(context.Context) AttemptResult {
		attempts++
		if attempts == 1 {
			return AttemptResult{Status: http.StatusTooManyRequests, RetryAfter: 90 * time.Second, Body: rateLimitBody, Err: errors.New("openai: http 429")}
		}
		return AttemptResult{}
	})
	if result.Err != nil || attempts != 2 {
		t.Fatalf("err = %v, attempts = %d; want success on the second attempt", result.Err, attempts)
	}
	if slept != 90*time.Second {
		t.Fatalf("slept = %v, want 1m30s", slept)
	}
	if len(ticked) == 0 || ticked[0] != 90*time.Second || ticked[len(ticked)-1] != 0 {
		t.Fatalf("ticks = %v; want a first tick of 1m30s and a final tick of 0", ticked)
	}
}

func TestRunRetriesStopsWhenInterruptedWhileWaiting(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancelCause(t.Context())
	defer cancel(nil)
	var ticked []time.Duration
	ctx = WithRetryWatch(ctx, RetryWatch{
		Attended: func() bool { return true },
		Tick: func(remaining time.Duration) {
			ticked = append(ticked, remaining)
			if len(ticked) == 2 {
				cancel(errors.New("interrupted"))
			}
		},
	})
	var attempts int
	done := make(chan AttemptResult, 1)
	go func() {
		done <- RunRetries(ctx, RetryOptions{Sleep: sleepContext}, func(context.Context) AttemptResult {
			attempts++
			return AttemptResult{Status: http.StatusTooManyRequests, RetryAfter: 90 * time.Second, Body: rateLimitBody, Err: errors.New("openai: http 429")}
		})
	}()
	var result AttemptResult
	select {
	case result = <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("RunRetries kept waiting after the caller's context ended")
	}
	if attempts != 1 || !errors.Is(result.Err, context.Canceled) {
		t.Fatalf("attempts = %d, err = %v; want 1 attempt and context.Canceled", attempts, result.Err)
	}
	if len(ticked) < 2 || ticked[len(ticked)-1] != 0 {
		t.Fatalf("ticks = %v; want the final tick to be 0", ticked)
	}
}
