package runner

import (
	"context"
	"errors"
	"testing"
	"time"
)

type aliveRunner struct {
	Runner
	onRead func()
}

func (r *aliveRunner) Read(_ context.Context, _ string, _ int64) (Output, error) {
	if r.onRead != nil {
		r.onRead()
	}
	return Output{Alive: true}, nil
}

func TestAwaitExitEndsWithTheCallerContext(t *testing.T) {
	ctx, cancel := context.WithCancelCause(t.Context())
	defer cancel(nil)
	r := &aliveRunner{onRead: func() { cancel(context.Canceled) }}
	err := AwaitExit(ctx, r, "p1", time.Minute)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("AwaitExit error = %v, want the caller's cancel cause", err)
	}
	if errors.Is(err, ErrExitTimeout) {
		t.Fatalf("AwaitExit reported a grace timeout after the caller cancelled: %v", err)
	}
}

func TestAwaitExitReportsTimeout(t *testing.T) {
	r := &aliveRunner{}
	err := AwaitExit(t.Context(), r, "p1", 50*time.Millisecond)
	if !errors.Is(err, ErrExitTimeout) {
		t.Fatalf("AwaitExit error = %v, want ErrExitTimeout", err)
	}
}
