package state

import (
	"encoding/json/jsontext"
	"fmt"
	"maps"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

type Intent interface{ message }

type (
	Connect  struct{ Endpoint client.EndpointID }
	OpenView struct {
		Window WindowID
		Thread ThreadKey
	}
	CloseView struct {
		Window WindowID
		Thread ThreadKey
	}
	CloseWindow struct{ Window WindowID }
	LoadOlder   struct{ Thread ThreadKey }
	Refresh     struct{ Thread ThreadKey }
	PutDraft    struct {
		Key   DraftKey
		Draft Draft
	}
	Send struct {
		Key   DraftKey
		Draft Draft
	}
	Steer struct {
		Key   DraftKey
		Draft Draft
	}
	Enqueue struct {
		Key   DraftKey
		Draft Draft
	}
	EditFollowUp struct {
		Key DraftKey
		ID  uint64
	}
	RemoveFollowUp struct {
		Key DraftKey
		ID  uint64
	}
	DiscardFollowUps struct{ Key DraftKey }
	Interrupt        struct {
		Window WindowID
		Thread ThreadKey
		Turn   string
	}
	Answer struct {
		Request RequestKey
		Result  jsontext.Value
	}
	RetryDetails  struct{ Request RequestKey }
	Retry         struct{ OperationID string }
	Check         struct{ OperationID string }
	Dismiss       struct{ OperationID string }
	Restore       struct{ OperationID string }
	DismissNotice struct{ ID uint64 }
)

func (i Connect) apply(m *machine) {
	ep := m.endpoint(i.Endpoint)
	if ep.live {
		return
	}
	ep.connecting = true
	m.emit(connectEffect{endpoint: ep.id})
}

func (i OpenView) apply(m *machine) {
	ep := m.endpoint(i.Thread.Endpoint)
	ts := m.watch(ep, i.Window, i.Thread)
	if ts == nil {
		return
	}
	m.use(ep)
	if ep.live && ts.proj == nil && ts.opening == 0 {
		m.open(ep, ts)
	}
}

func (m *machine) watch(ep *endpointState, window WindowID, key ThreadKey) *threadState {
	ts, ok := ep.threads[key.Thread]
	if !ok {
		if len(ep.threads) >= subscriptionLimit(ep) {
			m.notify(Notice{Endpoint: ep.id, Thread: key.Thread, Text: fmt.Sprintf("%s already shows %d threads; close one before opening another", m.label(ep.id), len(ep.threads))})
			return nil
		}
		ts = &threadState{key: key, interest: map[WindowID]int{}}
		ep.threads[key.Thread] = ts
		m.wantAttach(ep)
	}
	if ts.interest[window] == 0 {
		m.windows[window] = append(m.windows[window], key)
		m.dirty = true
	}
	ts.interest[window]++
	m.touch(ts)
	return ts
}

func subscriptionLimit(ep *endpointState) int {
	if ep.caps.Limits.Subscriptions > 0 {
		return ep.caps.Limits.Subscriptions
	}
	return defaultSubscriptions
}

func (i CloseView) apply(m *machine) {
	ep, ts := m.thread(i.Thread)
	if ts == nil || ts.interest[i.Window] == 0 {
		return
	}
	ts.interest[i.Window]--
	if ts.interest[i.Window] == 0 {
		delete(ts.interest, i.Window)
		m.unwatch(i.Window, i.Thread)
	}
	if len(ts.interest) == 0 {
		m.release(ep, ts)
	}
}

func (m *machine) unwatch(window WindowID, key ThreadKey) {
	m.windows[window] = slices.DeleteFunc(m.windows[window], func(k ThreadKey) bool { return k == key })
	if len(m.windows[window]) == 0 {
		delete(m.windows, window)
	}
	m.dirty = true
}

func (i CloseWindow) apply(m *machine) {
	for _, ep := range m.endpoints {
		for _, ts := range m.sortedThreads(ep) {
			if _, ok := ts.interest[i.Window]; !ok {
				continue
			}
			delete(ts.interest, i.Window)
			m.touch(ts)
			if len(ts.interest) == 0 {
				m.release(ep, ts)
			}
		}
	}
	inWindow := func(key DraftKey, _ []FollowUp) bool { return key.Window == i.Window }
	maps.DeleteFunc(m.queues, inWindow)
	maps.DeleteFunc(m.drafts, func(key DraftKey, _ Draft) bool { return key.Window == i.Window })
	m.reapWindowCalls(i.Window)
	delete(m.windows, i.Window)
	delete(m.started, i.Window)
	m.dirty = true
}

func (m *machine) reapWindowCalls(window WindowID) {
	maps.DeleteFunc(m.searches, func(key DraftKey, s *searchState) bool {
		if key.Window != window {
			return false
		}
		m.cancelCall(s.call)
		return true
	})
	maps.DeleteFunc(m.shells, func(key DraftKey, runs []*shellState) bool {
		if key.Window != window {
			return false
		}
		for _, run := range runs {
			m.cancelCall(run.call)
		}
		return true
	})
	maps.DeleteFunc(m.keywords, func(key DraftKey, state *keywordState) bool {
		if key.Window != window {
			return false
		}
		m.cancelCall(state.ticket)
		return true
	})
}

func (m *machine) cancelCall(ticket uint64) {
	if ticket != 0 {
		m.emit(cancelEffect{ticket: ticket})
	}
}

func (i LoadOlder) apply(m *machine) {
	ep, ts := m.thread(i.Thread)
	if ts == nil {
		return
	}
	m.use(ep)
	m.loadPage(ep, ts)
}

func (i Refresh) apply(m *machine) {
	ep, ts := m.thread(i.Thread)
	if ts == nil {
		return
	}
	ts.err = nil
	m.touch(ts)
	m.use(ep)
	if ep.live {
		m.open(ep, ts)
	}
}

func (i PutDraft) apply(m *machine) { m.keep(i.Key, i.Draft) }

func (i Send) apply(m *machine) {
	params := rpc.TurnStartParams{ThreadID: i.Key.Thread, Input: i.Draft.input()}
	m.submit(i.Key, i.Draft, &mutation{key: i.Key, method: methodTurnStart, params: params, payload: i.Draft})
}

func (i Steer) apply(m *machine) {
	turn := m.activeTurn(i.Key.thread())
	if turn == "" {
		m.notify(Notice{Endpoint: i.Key.Endpoint, Thread: i.Key.Thread, Text: "No turn is running to steer; send the message instead"})
		return
	}
	params := rpc.TurnSteerParams{ThreadID: i.Key.Thread, ExpectedTurnID: turn, Input: i.Draft.input()}
	m.submit(i.Key, i.Draft, &mutation{key: i.Key, method: methodTurnSteer, params: params, payload: i.Draft})
}

func (i Enqueue) apply(m *machine) { m.enqueue(i.Key, i.Draft) }

func (i EditFollowUp) apply(m *machine) {
	at := slices.IndexFunc(m.queues[i.Key], func(f FollowUp) bool { return f.ID == i.ID })
	if at < 0 {
		return
	}
	f := m.queues[i.Key][at]
	if m.place(i.Key, Draft{Text: f.Text, Attachments: f.Attachments}) {
		m.dropFollowUp(i.Key, i.ID)
	}
}

func (i RemoveFollowUp) apply(m *machine) { m.dropFollowUp(i.Key, i.ID) }

func (i DiscardFollowUps) apply(m *machine) { delete(m.queues, i.Key) }

func (i Interrupt) apply(m *machine) {
	turn := m.activeTurn(i.Thread)
	if turn == "" {
		return
	}
	if i.Turn != "" && i.Turn != turn {
		m.notify(Notice{Endpoint: i.Thread.Endpoint, Thread: i.Thread.Thread, Text: "That turn is no longer running; nothing was interrupted"})
		return
	}
	key := DraftKey{Endpoint: i.Thread.Endpoint, Window: i.Window, Thread: i.Thread.Thread}
	params := rpc.TurnInterruptParams{ThreadID: i.Thread.Thread, TurnID: turn}
	m.admit(m.endpoint(i.Thread.Endpoint), &mutation{key: key, method: methodTurnInterrupt, params: params})
}

func (m *machine) pendingRequest(key RequestKey) (*endpointState, *threadState, *request) {
	ep, ok := m.endpoints[key.Thread.Endpoint]
	if !ok {
		return nil, nil, nil
	}
	for _, ts := range m.sortedThreads(ep) {
		if ts.proj == nil {
			continue
		}
		if rq := ts.proj.request(key.Request); rq != nil && rq.owner(ts.key.Thread) == key.Thread.Thread {
			return ep, ts, rq
		}
	}
	return ep, nil, nil
}

func (i Answer) apply(m *machine) {
	ep, ts, rq := m.pendingRequest(i.Request)
	if rq == nil || !ep.live {
		m.notify(Notice{Endpoint: i.Request.Thread.Endpoint, Thread: i.Request.Thread.Thread, Text: "This request can no longer be answered from here"})
		return
	}
	to, waiting := ep.responders[rq.id]
	switch {
	case rq.params == nil || !waiting || to.answered && rq.warning == nil:
		m.notify(Notice{Endpoint: ep.id, Thread: i.Request.Thread.Thread, Text: "This request is no longer waiting for an answer"})
		return
	case to.answered:
		m.emit(respondEffect{link: ep.link, thread: ts.key, request: rq.id, result: i.Result})
	default:
		m.answer(to, reply{result: i.Result})
	}
	rq.answered, rq.warning = true, nil
	m.touch(ts)
	m.touchEndpoint(ep)
}

func (m *machine) responded(msg respondedMsg) {
	_, ts := m.thread(msg.thread)
	if ts == nil || ts.proj == nil {
		return
	}
	if rq := ts.proj.request(msg.request); rq != nil {
		rq.answered, rq.warning = false, &AnswerWarning{Message: msg.err.Error()}
		m.touch(ts)
	}
	m.notify(Notice{Endpoint: msg.thread.Endpoint, Thread: msg.thread.Thread, Text: fmt.Sprintf("Could not send the corrected answer: %v", msg.err)})
}

func (i RetryDetails) apply(m *machine) {
	ep, ts, rq := m.pendingRequest(i.Request)
	if rq != nil && !rq.loading {
		rq.err = nil
		m.loadDetails(ep, ts)
	}
}

func (i Retry) apply(m *machine) {
	mu := m.mutation(i.OperationID)
	if mu == nil || mu.state != MutationFailed {
		return
	}
	m.mutations = slices.DeleteFunc(m.mutations, func(x *mutation) bool { return x == mu })
	retried := &mutation{key: mu.key, method: mu.method, params: mu.params, payload: mu.payload}
	if !m.admit(m.endpoint(mu.endpoint()), retried) {
		m.mutations = append(m.mutations, mu)
	}
}

func (i Check) apply(m *machine) {
	mu := m.mutation(i.OperationID)
	if mu == nil && m.checkShell(i.OperationID) || mu == nil || mu.state != MutationUnknown || !mu.receipt {
		return
	}
	if ep := m.endpoint(mu.endpoint()); ep.live {
		m.readReceipt(ep, mu)
		return
	}
	m.use(m.endpoint(mu.endpoint()))
}

func (i Dismiss) apply(m *machine) {
	mu := m.mutation(i.OperationID)
	if mu == nil || mu.state == MutationDispatched || mu.state == MutationReconciling {
		return
	}
	m.mutations = slices.DeleteFunc(m.mutations, func(x *mutation) bool { return x == mu })
	if ep, ts := m.thread(mu.key.thread()); ts != nil {
		m.touch(ts)
		m.progress(ep, ts)
	}
}

func (i Restore) apply(m *machine) {
	mu := m.mutation(i.OperationID)
	if mu == nil || mu.payload.empty() || mu.state != MutationFailed && mu.state != MutationWaiting {
		return
	}
	if m.place(mu.key, mu.payload) {
		m.mutations = slices.DeleteFunc(m.mutations, func(x *mutation) bool { return x == mu })
	}
}

func (i DismissNotice) apply(m *machine) {
	m.notices = slices.DeleteFunc(m.notices, func(n Notice) bool { return n.ID == i.ID })
}
