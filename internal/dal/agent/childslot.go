package agent

import (
	"context"
	"errors"
	"sync"
)

type childSlots struct {
	mu       sync.Mutex
	groups   map[string]*slotGroup
	watchers sync.WaitGroup
}

func (s *childSlots) releaseWhenIdle(group *slotGroup, child *Thread) {
	s.watchers.Go(func() { group.giveWhenIdle(child) })
}

func (s *childSlots) join() { s.watchers.Wait() }

type slotGroup struct {
	admit chan struct{}
	free  chan struct{}
	mu    sync.Mutex
	refs  map[string]int
}

func (h *Host) slotGroup(root string) *slotGroup {
	h.slots.mu.Lock()
	defer h.slots.mu.Unlock()
	group, ok := h.slots.groups[root]
	if !ok {
		group = &slotGroup{admit: make(chan struct{}, 1), free: make(chan struct{}, maxChildren), refs: make(map[string]int)}
		h.slots.groups[root] = group
	}
	return group
}

func (t *Thread) slots() *slotGroup { return t.host.slotGroup(t.id) }

func (g *slotGroup) take(ctx context.Context, child string) error {
	if g.share(child) {
		return nil
	}
	select {
	case g.admit <- struct{}{}:
	case <-ctx.Done():
		return context.Cause(ctx)
	}
	defer func() { <-g.admit }()
	if g.share(child) {
		return nil
	}
	select {
	case g.free <- struct{}{}:
	case <-ctx.Done():
		return context.Cause(ctx)
	}
	g.mu.Lock()
	g.refs[child] = 1
	g.mu.Unlock()
	return nil
}

func (g *slotGroup) share(child string) bool {
	g.mu.Lock()
	defer g.mu.Unlock()
	refs := g.refs[child]
	if refs > 0 {
		g.refs[child] = refs + 1
	}
	return refs > 0
}

func (g *slotGroup) give(child string) {
	g.mu.Lock()
	defer g.mu.Unlock()
	switch refs := g.refs[child]; {
	case refs > 1:
		g.refs[child] = refs - 1
	case refs == 1:
		delete(g.refs, child)
		<-g.free
	}
}

func (g *slotGroup) giveWhenIdle(child *Thread) {
	updates := child.Subscribe()
	defer func() {
		child.Unsubscribe(updates)
		g.give(child.id)
	}()
	for {
		status, err := child.Status()
		if err != nil || !status.Busy {
			return
		}
		select {
		case _, open := <-updates:
			if !open {
				updates = child.Subscribe()
			}
		case <-child.stopped:
			return
		}
	}
}

func (h *Host) openRecovering(ctx context.Context, loaded loadedSession, overrides Overrides, release func() error) (*Thread, error) {
	created := loaded.created
	if created.Parent == "" || created.Advisor != "" || loaded.state.TurnID == "" || loaded.damage != nil {
		return h.openLocked(ctx, loaded, overrides, release)
	}
	slots := h.slotGroup(created.Parent)
	if err := slots.take(ctx, created.ID); err != nil {
		return nil, errors.Join(err, release())
	}
	t, err := h.openLocked(ctx, loaded, overrides, release)
	if err != nil {
		slots.give(created.ID)
		return nil, err
	}
	h.slots.releaseWhenIdle(slots, t)
	return t, nil
}
