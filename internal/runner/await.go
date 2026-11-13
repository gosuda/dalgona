package runner

import (
	"context"
	"errors"
	"time"
)

var ErrExitTimeout = errors.New("subprocess did not exit")

const exitPoll = 20 * time.Millisecond

func AwaitExit(ctx context.Context, r Runner, id string, grace time.Duration) error {
	waitCtx, cancel := context.WithTimeoutCause(ctx, grace, ErrExitTimeout)
	defer cancel()
	var lastErr error
	for {
		out, err := r.Read(waitCtx, id, 0)
		if err == nil && out.Exit != nil {
			return nil
		}
		if waitCtx.Err() != nil {
			if ctx.Err() != nil {
				return context.Cause(ctx)
			}
			return errors.Join(ErrExitTimeout, lastErr)
		}
		lastErr = err
		timer := time.NewTimer(exitPoll)
		select {
		case <-waitCtx.Done():
			timer.Stop()
		case <-timer.C:
		}
	}
}
