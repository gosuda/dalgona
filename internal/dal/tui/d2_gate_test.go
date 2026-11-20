package tui

import (
	"context"
	"errors"
	"testing"
	"testing/synctest"
	"time"
)

func enterD2(t *testing.T, g *d2Gate) func() {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), diagramBudget)
	t.Cleanup(cancel)
	leave, err := g.enter(ctx)
	if err != nil {
		t.Fatalf("enter on a free gate: %v", err)
	}
	return leave
}

func TestD2GateFailsAtOnceWhileASpentLayoutRuns(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		g := newD2Gate()
		leave := enterD2(t, g)
		time.Sleep(diagramBudget + time.Second)
		start := time.Now()
		ctx, cancel := context.WithTimeout(context.Background(), diagramBudget)
		defer cancel()
		if _, err := g.enter(ctx); !errors.Is(err, errD2Busy) {
			t.Fatalf("enter behind a layout past its limit = %v, want %v", err, errD2Busy)
		}
		if waited := time.Since(start); waited != 0 {
			t.Fatalf("enter behind a layout past its limit waited %v, want no wait", waited)
		}
		leave()
		next := enterD2(t, g)
		next()
	})
}

func TestD2GateWaitsForALayoutWithinItsLimit(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		g := newD2Gate()
		leave := enterD2(t, g)
		entered := make(chan error, 1)
		go func() {
			ctx, cancel := context.WithTimeout(context.Background(), diagramBudget)
			defer cancel()
			next, err := g.enter(ctx)
			if err == nil {
				next()
			}
			entered <- err
		}()
		synctest.Wait()
		select {
		case err := <-entered:
			t.Fatalf("enter returned %v while the first layout was within its limit", err)
		default:
		}
		leave()
		synctest.Wait()
		if err := <-entered; err != nil {
			t.Fatalf("enter after the first layout ended: %v", err)
		}
	})
}

func TestD2GateWaitEndsAtTheLimitOfTheWaiter(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		g := newD2Gate()
		leave := enterD2(t, g)
		defer leave()
		start := time.Now()
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		if _, err := g.enter(ctx); !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("enter that waits past its own limit = %v, want %v", err, context.DeadlineExceeded)
		}
		if waited := time.Since(start); waited != time.Second {
			t.Fatalf("enter waited %v, want 1s", waited)
		}
	})
}
