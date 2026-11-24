package platform

import (
	"context"
	"sync"
)

type Generation uint64

type Lifetime struct {
	mu       sync.Mutex
	issued   Generation
	current  Generation
	cancel   context.CancelFunc
	detaches []func()
}

func (l *Lifetime) Begin(ctx context.Context) (Generation, context.Context) {
	l.End()
	pending, cancel := context.WithCancel(ctx)
	l.mu.Lock()
	defer l.mu.Unlock()
	l.issued++
	l.current, l.cancel = l.issued, cancel
	return l.current, pending
}

func (l *Lifetime) End() {
	l.mu.Lock()
	cancel, detaches := l.cancel, l.detaches
	l.current, l.cancel, l.detaches = 0, nil, nil
	l.mu.Unlock()
	if cancel == nil {
		return
	}
	cancel()
	for i := len(detaches) - 1; i >= 0; i-- {
		detaches[i]()
	}
}

func (l *Lifetime) Current(g Generation) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	return g != 0 && g == l.current
}

func (l *Lifetime) Attach(g Generation, detach func()) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	if g == 0 || g != l.current {
		return false
	}
	l.detaches = append(l.detaches, detach)
	return true
}
