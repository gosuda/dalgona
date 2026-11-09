package model

import (
	"context"
	"errors"
	"net"
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
