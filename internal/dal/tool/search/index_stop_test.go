package search

import (
	"context"
	"errors"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/dal/runner"
)

type delayedBuildExit struct {
	runner.Runner
	delay   time.Duration
	exitAt  time.Time
	signals int
}

func (r *delayedBuildExit) Signal(ctx context.Context, _ string, signal runner.Signal) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if signal != runner.SignalKill {
		return errors.New("expected a kill signal")
	}
	r.signals++
	r.exitAt = time.Now().Add(r.delay)
	return nil
}

func (r *delayedBuildExit) Read(context.Context, string, int64) (runner.Output, error) {
	if r.signals == 0 || time.Now().Before(r.exitAt) {
		return runner.Output{Alive: true}, nil
	}
	return runner.Output{Exit: &runner.Exit{Code: 137, Signal: "killed", EndedAt: r.exitAt}}, nil
}

func TestStopBuilderWaitsForDelayedExitRecord(t *testing.T) {
	cases := []struct {
		name      string
		remaining time.Duration
		delay     time.Duration
	}{
		{"expired build still allows delayed record", -time.Second, 6 * time.Second},
		{"remaining build budget extends cleanup", time.Minute, 40 * time.Second},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				ctx, cancel := context.WithTimeout(context.Background(), tc.remaining)
				defer cancel()
				run := &delayedBuildExit{delay: tc.delay}
				cause := errors.New("read failed")
				started := time.Now()
				err := stopBuilder(ctx, run, "index-build", cause)
				if !errors.Is(err, cause) || errors.Is(err, errChildRunning) {
					t.Fatalf("stopBuilder = %v, want the original failure after confirmed exit", err)
				}
				if run.signals != 1 || time.Since(started) < tc.delay {
					t.Fatalf("signals = %d, elapsed = %s, want one kill and the delayed exit", run.signals, time.Since(started))
				}
			})
		})
	}
}

func TestStopBuilderKeepsUnconfirmedChildAfterCleanupBudget(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		cancel()
		run := &delayedBuildExit{delay: 31 * time.Second}
		cause := errors.New("read failed")
		started := time.Now()
		err := stopBuilder(ctx, run, "index-build", cause)
		if elapsed := time.Since(started); elapsed != 30*time.Second {
			t.Fatalf("cleanup lasted %s, want the 30-second floor", elapsed)
		}
		if !errors.Is(err, cause) || !errors.Is(err, errChildRunning) || !errors.Is(err, runner.ErrExitTimeout) {
			t.Fatalf("stopBuilder = %v, want the original failure and an unconfirmed child", err)
		}
		if run.signals != 1 {
			t.Fatalf("signals = %d, want one kill despite the canceled build", run.signals)
		}
	})
}
