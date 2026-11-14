package agent

import (
	"context"
	"errors"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/model"
)

var (
	errStubModels = errors.New("agent: stub models unavailable")
	errStubStream = errors.New("agent: stub stream unused")
)

type stubModelLister struct {
	mu       sync.Mutex
	calls    atomic.Int64
	models   []model.Model
	err      error
	deadline atomic.Bool
}

func (s *stubModelLister) Models(ctx context.Context) ([]model.Model, error) {
	s.calls.Add(1)
	_, ok := ctx.Deadline()
	s.deadline.Store(ok)
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.models, s.err
}

func (s *stubModelLister) Stream(context.Context, model.Request) (model.Stream, error) {
	return nil, errStubStream
}

func modelsRegistry(t *testing.T, provider model.Provider) *model.Registry {
	t.Helper()
	registry := model.NewRegistry()
	if err := registry.Register("stub", provider); err != nil {
		t.Fatalf("agent: register provider: %v", err)
	}
	return registry
}

func TestProviderModelsCachesFailures(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	now := time.Now()
	h.host.clock = func() time.Time { return now }
	lister := &stubModelLister{err: errStubModels}
	registry := modelsRegistry(t, lister)
	if got := h.host.providerModels(t.Context(), registry, "stub"); len(got) != 0 {
		t.Fatalf("agent: providerModels = %v, want empty", got)
	}
	if got := h.host.providerModels(t.Context(), registry, "stub"); len(got) != 0 {
		t.Fatalf("agent: providerModels = %v, want empty", got)
	}
	if calls := lister.calls.Load(); calls != 1 {
		t.Fatalf("agent: provider models calls = %d, want 1 cached failure", calls)
	}
	now = now.Add(2 * negativeModelsTTL)
	if got := h.host.providerModels(t.Context(), registry, "stub"); len(got) != 0 {
		t.Fatalf("agent: providerModels = %v, want empty", got)
	}
	if calls := lister.calls.Load(); calls != 2 {
		t.Fatalf("agent: provider models calls = %d, want refresh after TTL", calls)
	}
}

func TestProviderModelsBoundsLookup(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	lister := &stubModelLister{models: []model.Model{{ID: "stub-a"}}}
	registry := modelsRegistry(t, lister)
	if got := h.host.providerModels(t.Context(), registry, "stub"); len(got) != 1 {
		t.Fatalf("agent: providerModels = %v, want one model", got)
	}
	if !lister.deadline.Load() {
		t.Fatal("agent: provider models lookup ran without a timeout")
	}
	if got := h.host.providerModels(t.Context(), registry, "stub"); len(got) != 1 {
		t.Fatalf("agent: providerModels = %v, want one model", got)
	}
	if calls := lister.calls.Load(); calls != 1 {
		t.Fatalf("agent: provider models calls = %d, want 1 cached success", calls)
	}
}

func TestProviderModelsSkipsCacheOnCallerCancel(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	now := time.Now()
	h.host.clock = func() time.Time { return now }
	lister := &stubModelLister{err: errStubModels}
	registry := modelsRegistry(t, lister)
	ctx, cancel := context.WithCancelCause(t.Context())
	cancel(context.Canceled)
	if got := h.host.providerModels(ctx, registry, "stub"); len(got) != 0 {
		t.Fatalf("agent: providerModels = %v, want empty", got)
	}
	if got := h.host.providerModels(t.Context(), registry, "stub"); len(got) != 0 {
		t.Fatalf("agent: providerModels = %v, want empty", got)
	}
	if calls := lister.calls.Load(); calls != 2 {
		t.Fatalf("agent: provider models calls = %d, want 2 without caching caller cancel", calls)
	}
}

type blockingModelLister struct {
	calls   atomic.Int64
	release chan struct{}
}

func (s *blockingModelLister) Models(context.Context) ([]model.Model, error) {
	s.calls.Add(1)
	<-s.release
	return []model.Model{{ID: "stub-a"}}, nil
}

func (s *blockingModelLister) Stream(context.Context, model.Request) (model.Stream, error) {
	return nil, errStubStream
}

func resetModelTimer(timer *time.Timer, d time.Duration) {
	if !timer.Stop() {
		select {
		case <-timer.C:
		default:
		}
	}
	timer.Reset(d)
}

func assertSingleModel(t *testing.T, got []model.Model) {
	t.Helper()
	if len(got) != 1 {
		t.Fatalf("agent: providerModels = %v, want one model", got)
	}
}

func collectOneModel(t *testing.T, done <-chan []model.Model, timer *time.Timer) {
	t.Helper()
	select {
	case got := <-done:
		assertSingleModel(t, got)
		resetModelTimer(timer, 5*time.Second)
	case <-timer.C:
		t.Fatal("agent: concurrent lookup did not return")
	}
}

func collectModels(t *testing.T, done chan []model.Model, callers int) {
	t.Helper()
	timer := time.NewTimer(5 * time.Second)
	defer timer.Stop()
	for range callers {
		collectOneModel(t, done, timer)
	}
}

func TestProviderModelsCollapsesConcurrentLookups(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		h := newHarness(t)
		lister := &blockingModelLister{release: make(chan struct{})}
		registry := modelsRegistry(t, lister)
		const callers = 8
		done := make(chan []model.Model, callers)
		for range callers {
			go func() {
				done <- h.host.providerModels(t.Context(), registry, "stub")
			}()
		}
		synctest.Wait()
		if calls := lister.calls.Load(); calls != 1 {
			close(lister.release)
			t.Fatalf("agent: provider models calls while %d lookups wait = %d, want 1", callers, calls)
		}
		close(lister.release)
		collectModels(t, done, callers)
		if calls := lister.calls.Load(); calls != 1 {
			t.Fatalf("agent: provider models calls = %d, want 1 collapsed lookup", calls)
		}
	})
}
