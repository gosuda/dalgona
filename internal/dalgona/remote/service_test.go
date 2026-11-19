package remote

import (
	"context"
	"errors"
	"io"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc"
)

type stubService struct{}

func (stubService) Create(_ context.Context, _ sandboxsvc.Spec) (string, error) {
	return "", errors.New("stub: no boxes")
}

func (stubService) Attach(_ context.Context, _ string) (string, error) {
	return "", errors.New("stub: no boxes")
}

func (stubService) Destroy(_ context.Context, _ string) error {
	return errors.New("stub: no boxes")
}

func (stubService) Connect(_ context.Context, _ string, _ sandboxsvc.Endpoint) (io.ReadWriteCloser, error) {
	return nil, errors.New("stub: no boxes")
}

type outcome struct {
	svc sandboxsvc.Service
	err error
}

func bindAsync(ctx context.Context, b *Binder, spec config.RunnerSpec) <-chan outcome {
	ch := make(chan outcome, 1)
	go func() {
		svc, err := b.service(ctx, spec)
		ch <- outcome{svc: svc, err: err}
	}()
	return ch
}

func requireOutcome(t *testing.T, ch <-chan outcome, what string) outcome {
	t.Helper()
	var got outcome
	select {
	case got = <-ch:
	case <-time.After(5 * time.Second):
		t.Fatalf("%s did not return within 5 seconds", what)
	}
	return got
}

func requireShared(t *testing.T, got outcome, want sandboxsvc.Service, what string) {
	t.Helper()
	if got.err != nil || got.svc != want {
		t.Fatalf("%s = (%v, %v), want the shared service", what, got.svc, got.err)
	}
}

type blockingService struct {
	release chan struct{}
	entered chan struct{}
	mu      sync.Mutex
	calls   int
	fast    stubService
}

func blockingBinder() (*Binder, *blockingService) {
	svc := &blockingService{release: make(chan struct{}), entered: make(chan struct{}, 1)}
	b := NewBinder(Options{Config: config.Config{}})
	b.newSvc = svc.build
	return b, svc
}

func (s *blockingService) build(ctx context.Context, spec config.RunnerSpec) (sandboxsvc.Service, error) {
	s.mu.Lock()
	s.calls++
	s.mu.Unlock()
	if spec.Service != "slow" {
		return s.fast, nil
	}
	return s.wait(ctx)
}

func (s *blockingService) wait(ctx context.Context) (sandboxsvc.Service, error) {
	select {
	case s.entered <- struct{}{}:
	default:
	}
	select {
	case <-s.release:
		return s.fast, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

func (s *blockingService) constructed() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.calls
}

func TestServiceSharesInFlightConstructionPerSpec(t *testing.T) {
	b, svc := blockingBinder()
	slow := config.RunnerSpec{Service: "slow"}
	first := bindAsync(t.Context(), b, slow)
	<-svc.entered
	second := bindAsync(t.Context(), b, slow)
	select {
	case got := <-second:
		t.Fatalf("second waiter returned (%v, %v) before release", got.svc, got.err)
	case <-time.After(200 * time.Millisecond):
	}
	close(svc.release)
	requireShared(t, requireOutcome(t, first, "first waiter"), svc.fast, "first waiter")
	requireShared(t, requireOutcome(t, second, "second waiter"), svc.fast, "second waiter")
	if got := svc.constructed(); got != 1 {
		t.Fatalf("constructed %d services, want 1", got)
	}
}

func TestServiceConstructionDoesNotBlockOtherSpecs(t *testing.T) {
	b, svc := blockingBinder()
	ctx, cancel := context.WithCancel(t.Context())
	slowDone := bindAsync(ctx, b, config.RunnerSpec{Service: "slow"})
	<-svc.entered
	start := time.Now()
	got := requireOutcome(t, bindAsync(t.Context(), b, config.RunnerSpec{Service: "fast"}), "fast service")
	requireShared(t, got, svc.fast, "fast service")
	if elapsed := time.Since(start); elapsed > 5*time.Second {
		t.Fatalf("fast service returned after %v while the slow dial held no lock", elapsed)
	}
	cancel()
	if got := requireOutcome(t, slowDone, "slow service"); !errors.Is(got.err, context.Canceled) {
		t.Fatalf("slow service error = %v, want context.Canceled", got.err)
	}
}

func waitersOf(b *Binder, spec config.RunnerSpec) int32 {
	b.mu.Lock()
	defer b.mu.Unlock()
	if call, ok := b.calls[spec]; ok {
		return call.waiters.Load()
	}
	return -1
}

func requireJoined(t *testing.T, b *Binder, spec config.RunnerSpec) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for waitersOf(b, spec) != 1 {
		if time.Now().After(deadline) {
			t.Fatal("second caller did not join the shared build within 5 seconds")
		}
		time.Sleep(time.Millisecond)
	}
}

func TestServiceCancelledBuilderDoesNotPoisonWaiter(t *testing.T) {
	b, svc := blockingBinder()
	slow := config.RunnerSpec{Service: "slow"}
	builderCtx, cancel := context.WithCancel(t.Context())
	first := bindAsync(builderCtx, b, slow)
	<-svc.entered
	second := bindAsync(t.Context(), b, slow)
	requireJoined(t, b, slow)
	cancel()
	if got := requireOutcome(t, first, "cancelled builder"); !errors.Is(got.err, context.Canceled) {
		t.Fatalf("cancelled builder error = %v, want context.Canceled", got.err)
	}
	close(svc.release)
	requireShared(t, requireOutcome(t, second, "surviving waiter"), svc.fast, "surviving waiter")
	if got := svc.constructed(); got != 2 {
		t.Fatalf("constructed %d services, want 2", got)
	}
}
