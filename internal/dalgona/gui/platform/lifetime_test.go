package platform

import (
	"context"
	"errors"
	"sync"
	"sync/atomic"
	"testing"
)

func TestLifetimeEndCancelsOperationsBeforeReleasingAdapters(t *testing.T) {
	var l Lifetime
	g, ctx := l.Begin(t.Context())
	var observed error
	if !l.Attach(g, func() { observed = ctx.Err() }) {
		t.Fatal("Attach refused the current generation")
	}
	l.End()
	if !errors.Is(observed, context.Canceled) {
		t.Fatalf("adapter release observed %v, want the pending operation already cancelled", observed)
	}
	if l.Current(g) {
		t.Fatal("an ended generation still reports as current")
	}
}

func TestLifetimeNewViewInvalidatesEarlierHandles(t *testing.T) {
	var l Lifetime
	first, firstCtx := l.Begin(t.Context())
	released := 0
	l.Attach(first, func() { released++ })
	second, secondCtx := l.Begin(t.Context())
	if second == first {
		t.Fatalf("a new view reused generation %d", first)
	}
	if firstCtx.Err() == nil || released != 1 {
		t.Fatalf("earlier generation: context %v, released %d times; want cancelled and released once", firstCtx.Err(), released)
	}
	if l.Current(first) || l.Attach(first, func() {}) {
		t.Fatal("a stale generation was accepted")
	}
	if !l.Current(second) || secondCtx.Err() != nil {
		t.Fatal("the new generation is not live")
	}
}

func TestLifetimeReleasesEachAdapterOnce(t *testing.T) {
	var l Lifetime
	g, _ := l.Begin(t.Context())
	var released [3]int
	for i := range released {
		l.Attach(g, func() { released[i]++ })
	}
	l.End()
	l.End()
	l.Begin(t.Context())
	l.End()
	if released != [3]int{1, 1, 1} {
		t.Fatalf("release counts %v, want each adapter released exactly once", released)
	}
}

func TestLifetimeNativeCallbacksRaceWithClose(t *testing.T) {
	var l Lifetime
	g, _ := l.Begin(t.Context())
	var attached, released atomic.Int64
	var wg sync.WaitGroup
	for range 8 {
		wg.Go(func() {
			for range 200 {
				if l.Attach(g, func() { released.Add(1) }) {
					attached.Add(1)
				}
				l.Current(g)
			}
		})
	}
	wg.Go(l.End)
	wg.Wait()
	l.End()
	if attached.Load() != released.Load() {
		t.Fatalf("%d adapters attached but %d released", attached.Load(), released.Load())
	}
}
