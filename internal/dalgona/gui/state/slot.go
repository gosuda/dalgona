package state

import (
	"cmp"
	"encoding/json/jsontext"
	"fmt"
	"maps"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

const finishedSlotLimit = 64

type (
	CallID    string
	CallState uint8
)

const (
	CallRunning CallState = iota
	CallSucceeded
	CallFailed
	CallUnknown
)

type (
	Call struct {
		ID        CallID
		Endpoint  client.EndpointID
		Method    string
		Params    any
		Operation bool
	}
	CancelCall struct{ ID CallID }
	CallView   struct {
		ID         CallID
		Endpoint   client.EndpointID
		Generation client.Generation
		State      CallState
		Result     jsontext.Value
		Err        error
	}
	callSlot struct {
		view       CallView
		method     string
		params     any
		operation  string
		receipt    bool
		ticket     uint64
		dispatched bool
		finished   uint64
	}
	slotCall        struct{ id CallID }
	slotReceiptCall struct{ id CallID }
)

func (i Call) apply(m *machine) {
	if s, ok := m.slots[i.ID]; ok && s.view.State == CallRunning {
		return
	}
	ep := m.endpoint(i.Endpoint)
	s := &callSlot{view: CallView{ID: i.ID, Endpoint: ep.id, State: CallRunning}, method: i.Method, params: i.Params}
	if i.Operation {
		s.operation = rpc.NewOperationID()
	}
	m.slots[i.ID] = s
	if refusesSend(ep.phase) {
		m.finishSlot(s, CallFailed, nil, fmt.Errorf("%s is %s; reconnect first", m.label(ep.id), ep.phase))
		return
	}
	m.use(ep)
	if ep.live {
		m.dispatchSlot(ep, s)
	}
}

func (i CancelCall) apply(m *machine) {
	s, ok := m.slots[i.ID]
	if !ok {
		return
	}
	if s.operation == "" && s.ticket != 0 {
		m.emit(cancelEffect{ticket: s.ticket})
	}
	delete(m.slots, i.ID)
}

func (m *machine) dispatchSlot(ep *endpointState, s *callSlot) {
	s.view.Generation, s.dispatched = ep.link.Generation, true
	if s.operation == "" {
		s.ticket = m.call(ep, slotCall{id: s.view.ID}, s.method, s.params)
		return
	}
	s.receipt = slices.Contains(ep.caps.ReceiptMethods, s.method)
	link, params, receipt := ep.link, s.params, s.receipt
	op := rpc.Operation{Method: s.method, OperationID: s.operation, Endpoint: string(ep.id), Generation: uint64(link.Generation)}
	s.ticket = m.spawn(ep, slotCall{id: s.view.ID}, func(ticket uint64) effect {
		if receipt {
			return operationEffect{ticket: ticket, link: link, op: op, params: params, deadline: pageDeadline}
		}
		return callEffect{ticket: ticket, link: link, method: op.Method, params: params, deadline: pageDeadline}
	})
}

func (m *machine) dispatchSlots(ep *endpointState) {
	for _, id := range slices.Sorted(maps.Keys(m.slots)) {
		s := m.slots[id]
		switch {
		case s.view.Endpoint != ep.id:
		case s.view.State == CallRunning && !s.dispatched:
			m.dispatchSlot(ep, s)
		case s.view.State == CallUnknown && s.receipt:
			s.ticket = m.call(ep, slotReceiptCall{id: id}, methodOperationRead, operationParams{OperationID: s.operation})
		}
	}
}

func (m *machine) abandonSlots(ep *endpointState, cause error) {
	for _, s := range m.slots {
		if s.view.Endpoint == ep.id && s.view.State == CallRunning && !s.dispatched {
			m.finishSlot(s, CallFailed, nil, cause)
		}
	}
}

func (c slotCall) resume(m *machine, r resultMsg) {
	s, ok := m.slots[c.id]
	if !ok || s.ticket != r.ticket {
		return
	}
	s.ticket = 0
	switch outcome(s.operation, r.err) {
	case outcomeSucceeded:
		m.finishSlot(s, CallSucceeded, r.raw, nil)
	case outcomeUndispatched:
		s.dispatched = false
	case outcomeFailed:
		m.finishSlot(s, CallFailed, nil, r.err)
	case outcomeRunning, outcomeUnknown:
		m.uncertainSlot(s, r.err)
	}
}

func (m *machine) uncertainSlot(s *callSlot, err error) {
	if s.operation == "" {
		m.finishSlot(s, CallFailed, nil, err)
		return
	}
	m.finishSlot(s, CallUnknown, nil, err)
	if ep := m.endpoints[s.view.Endpoint]; s.receipt && ep.live {
		s.ticket = m.call(ep, slotReceiptCall{id: s.view.ID}, methodOperationRead, operationParams{OperationID: s.operation})
	}
}

func (c slotReceiptCall) resume(m *machine, r resultMsg) {
	s, ok := m.slots[c.id]
	if !ok || s.ticket != r.ticket {
		return
	}
	s.ticket = 0
	if data, coded := rpc.ErrorDataOf(r.err); coded && data.Kind == rpc.ErrorNotFound {
		m.finishSlot(s, CallFailed, nil, errNeverStarted)
		return
	}
	record, err := decode[rpc.OperationRecord](r.raw)
	switch {
	case r.err != nil:
		s.view.Err = r.err
	case err != nil:
		s.view.Err = err
	case record.State == "succeeded":
		m.finishSlot(s, CallSucceeded, record.Result, nil)
	case record.State == "failed":
		m.finishSlot(s, CallFailed, nil, recordError(record))
	}
}

func (m *machine) finishSlot(s *callSlot, state CallState, result jsontext.Value, err error) {
	s.view.State, s.view.Result, s.view.Err, s.finished = state, result, err, m.next()
	var finished []*callSlot
	for _, other := range m.slots {
		if other.view.Endpoint == s.view.Endpoint && other.finished != 0 {
			finished = append(finished, other)
		}
	}
	slices.SortFunc(finished, func(a, b *callSlot) int { return cmp.Compare(a.finished, b.finished) })
	for _, old := range finished[:max(0, len(finished)-finishedSlotLimit)] {
		delete(m.slots, old.view.ID)
	}
}
