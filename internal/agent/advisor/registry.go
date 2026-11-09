package advisor

import (
	"context"
	"errors"
	"sync"
	"sync/atomic"

	"github.com/gosuda/dalgona/internal/agent"
)

var ErrRegistryClosed = errors.New("advisor: registry closed")

type Registry struct {
	mu      sync.Mutex
	closed  bool
	threads map[string]*registered
}

type registered struct {
	attach  sync.Mutex
	optedIn bool
	runtime atomic.Pointer[Runtime]
}

func NewRegistry() *Registry {
	return &Registry{threads: make(map[string]*registered)}
}

func (r *Registry) Get(threadID string) *Runtime {
	r.mu.Lock()
	entry := r.threads[threadID]
	r.mu.Unlock()
	if entry == nil {
		return nil
	}
	runtime := entry.runtime.Load()
	if runtime == nil {
		return nil
	}
	if _, err := runtime.primary.Status(); errors.Is(err, agent.ErrClosed) {
		return nil
	}
	return runtime
}

func (r *Registry) Attach(ctx context.Context, host *agent.Host, thread *agent.Thread) (*Runtime, error) {
	return r.attach(ctx, host, thread, false)
}

func (r *Registry) AttachChild(ctx context.Context, host *agent.Host, child *agent.Thread) (*Runtime, error) {
	return r.attach(ctx, host, child, true)
}

func (r *Registry) attach(ctx context.Context, host *agent.Host, thread *agent.Thread, optIn bool) (*Runtime, error) {
	entry, err := r.entry(thread.ID())
	if err != nil {
		return nil, err
	}
	entry.attach.Lock()
	defer entry.attach.Unlock()
	entry.optedIn = entry.optedIn || optIn
	if current := entry.runtime.Load(); current != nil && current.primary == thread {
		return current, nil
	}
	if closeErr := entry.closeRuntime(); closeErr != nil {
		return nil, closeErr
	}
	cfg := host.Config()
	cfg.Advisor.Subagents = cfg.Advisor.Subagents || entry.optedIn
	runtime, err := Attach(ctx, host, thread, cfg)
	if err != nil {
		return nil, err
	}
	if !r.store(entry, runtime) {
		return nil, errors.Join(ErrRegistryClosed, runtime.Close())
	}
	return runtime, nil
}

func (r *Registry) entry(threadID string) (*registered, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return nil, ErrRegistryClosed
	}
	entry := r.threads[threadID]
	if entry == nil {
		entry = &registered{}
		r.threads[threadID] = entry
	}
	return entry, nil
}

func (r *Registry) store(entry *registered, runtime *Runtime) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return false
	}
	entry.runtime.Store(runtime)
	return true
}

func (e *registered) closeRuntime() error {
	if runtime := e.runtime.Swap(nil); runtime != nil {
		return runtime.Close()
	}
	return nil
}

func (r *Registry) Close() error {
	r.mu.Lock()
	threads := r.threads
	r.closed, r.threads = true, nil
	r.mu.Unlock()
	errs := make([]error, 0, len(threads))
	for _, entry := range threads {
		entry.attach.Lock()
		errs = append(errs, entry.closeRuntime())
		entry.attach.Unlock()
	}
	return errors.Join(errs...)
}
