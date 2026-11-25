//go:build gui

package decision

import (
	"cmp"
	"slices"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type pending struct {
	view     state.RequestView
	owner    *state.ThreadView
	link     client.Link
	child    string
	endpoint string
}

type scene struct {
	queue    []pending
	unknown  []pending
	resolved []state.ResolvedRequest
	covered  map[state.ThreadKey]bool
}

type threads func(state.ThreadKey) (*state.ThreadView, bool)

func sceneOf(v *state.View, open state.ThreadKey) scene {
	if v == nil {
		return scene{}
	}
	tv, ok := v.Thread(open)
	if !ok {
		return scene{}
	}
	return collect(tv, v.Thread, endpointLabel(v.Endpoints(), open.Endpoint))
}

func endpointLabel(endpoints []state.EndpointView, id client.EndpointID) string {
	for _, e := range endpoints {
		if e.ID == id && e.Label != "" {
			return e.Label
		}
	}
	return string(id)
}

func collect(open *state.ThreadView, lookup threads, label string) scene {
	sc := scene{covered: map[state.ThreadKey]bool{}}
	seen := map[state.RequestKey]bool{}
	holders := []*state.ThreadView{open}
	for _, c := range open.Children {
		if ct, ok := lookup(c.Thread); ok {
			holders = append(holders, ct)
		}
		if open.Live {
			sc.covered[c.Thread] = true
		}
	}
	for _, h := range holders {
		sc.resolved = append(sc.resolved, h.Resolved...)
		if !h.Live {
			continue
		}
		sc.covered[h.Key] = true
		for _, r := range h.Requests {
			if seen[r.Key] {
				continue
			}
			seen[r.Key] = true
			sc.place(pending{view: r, owner: ownerOf(h, r.Key.Thread, lookup), link: h.Link, child: childName(open, r.Key.Thread, lookup), endpoint: label})
		}
	}
	slices.SortStableFunc(sc.queue, func(a, b pending) int { return cmp.Compare(a.view.Arrival, b.view.Arrival) })
	return sc
}

func (sc *scene) place(p pending) {
	if known(p.view.Method) {
		sc.queue = append(sc.queue, p)
		return
	}
	sc.unknown = append(sc.unknown, p)
}

func ownerOf(holder *state.ThreadView, thread state.ThreadKey, lookup threads) *state.ThreadView {
	if holder.Key == thread {
		return holder
	}
	if tv, ok := lookup(thread); ok {
		return tv
	}
	return nil
}

func childName(open *state.ThreadView, thread state.ThreadKey, lookup threads) string {
	if thread == open.Key {
		return ""
	}
	if tv, ok := lookup(thread); ok && tv.Thread.Name != "" {
		return tv.Thread.Name
	}
	return thread.Thread
}

func (sc scene) resolution(key state.RequestKey) (state.ResolvedRequest, bool) {
	for i := len(sc.resolved) - 1; i >= 0; i-- {
		if sc.resolved[i].Key == key {
			return sc.resolved[i], true
		}
	}
	return state.ResolvedRequest{}, false
}

func (sc scene) queued(key state.RequestKey) bool {
	return slices.ContainsFunc(sc.queue, func(p pending) bool { return p.view.Key == key })
}
