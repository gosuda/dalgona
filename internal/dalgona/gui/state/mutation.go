package state

import (
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

const (
	pendingMutationLimit = 64
	methodTurnStart      = "turn/start"
	methodTurnSteer      = "turn/steer"
	methodTurnInterrupt  = "turn/interrupt"
	methodThreadStart    = "thread/start"
)

type MutationState uint8

const (
	MutationWaiting MutationState = iota
	MutationDispatched
	MutationUnknown
	MutationReconciling
	MutationFailed
)

var (
	errNeverStarted    = errors.New("the operation never started")
	errOperationFailed = errors.New("the operation failed")
)

type mutation struct {
	id       string
	key      DraftKey
	method   string
	params   any
	payload  Draft
	receipt  bool
	state    MutationState
	link     client.Link
	ticket   uint64
	err      error
	progress string
}

type (
	mutationCall struct{ id string }
	receiptCall  struct {
		id   string
		link client.Link
	}
)

func (mu *mutation) endpoint() client.EndpointID { return mu.key.Endpoint }

func (mu *mutation) unresolved() bool { return mu.state != MutationFailed }

func (m *machine) mutation(id string) *mutation {
	for _, mu := range m.mutations {
		if mu.id == id {
			return mu
		}
	}
	return nil
}

func (m *machine) pending(ep client.EndpointID) int {
	n := 0
	for _, mu := range m.mutations {
		if mu.endpoint() == ep && mu.unresolved() {
			n++
		}
	}
	return n
}

func (m *machine) admit(ep *endpointState, mu *mutation) bool {
	if refusesSend(ep.phase) {
		m.notify(Notice{Endpoint: ep.id, Thread: mu.key.Thread, Text: fmt.Sprintf("%s is %s; reconnect before sending", m.label(ep.id), ep.phase)})
		return false
	}
	if m.pending(ep.id) >= pendingMutationLimit {
		m.notify(Notice{Endpoint: ep.id, Thread: mu.key.Thread, Text: fmt.Sprintf("%s has %d unfinished actions; wait for them or resolve them first", m.label(ep.id), pendingMutationLimit)})
		return false
	}
	mu.id = rpc.NewOperationID()
	m.mutations = append(m.mutations, mu)
	m.use(ep)
	m.dispatchWaiting(ep)
	return true
}

func (m *machine) dispatchWaiting(ep *endpointState) {
	for _, mu := range m.mutations {
		if ep.workers >= workerLimit || !ep.live {
			return
		}
		if mu.endpoint() == ep.id && mu.state == MutationWaiting {
			m.dispatch(ep, mu)
		}
	}
}

func (m *machine) dispatch(ep *endpointState, mu *mutation) {
	mu.state, mu.link, mu.err = MutationDispatched, ep.link, nil
	mu.receipt = slices.Contains(ep.caps.ReceiptMethods, mu.method)
	link, params := ep.link, mu.params
	op := rpc.Operation{Method: mu.method, OperationID: mu.id, Endpoint: string(ep.id), Generation: uint64(ep.link.Generation)}
	receipt := mu.receipt
	mu.ticket = m.spawn(ep, mutationCall{id: mu.id}, func(ticket uint64) effect {
		if receipt {
			return operationEffect{ticket: ticket, link: link, op: op, params: params, deadline: pageDeadline}
		}
		return callEffect{ticket: ticket, link: link, method: op.Method, params: params, deadline: pageDeadline}
	})
}

func (c mutationCall) resume(m *machine, r resultMsg) {
	mu := m.mutation(c.id)
	if mu == nil || mu.ticket != r.ticket {
		return
	}
	m.conclude(mu, r)
	if ep, ok := m.endpoints[mu.endpoint()]; ok {
		m.dispatchWaiting(ep)
	}
}

func (m *machine) conclude(mu *mutation, r resultMsg) {
	switch outcome(mu.id, r.err) {
	case outcomeSucceeded:
		m.succeed(mu, r.raw)
	case outcomeFailed:
		m.fail(mu, r.err)
	case outcomeUndispatched:
		if mu.state == MutationDispatched {
			mu.state = MutationWaiting
		}
	case outcomeRunning:
		mu.state, mu.progress = MutationUnknown, "running"
	case outcomeUnknown:
		mu.state = MutationUnknown
	}
}

type mutationOutcome uint8

const (
	outcomeSucceeded mutationOutcome = iota
	outcomeFailed
	outcomeUndispatched
	outcomeRunning
	outcomeUnknown
)

func outcome(id string, err error) mutationOutcome {
	if err == nil {
		return outcomeSucceeded
	}
	if errors.Is(err, client.ErrStaleLink) {
		return outcomeUndispatched
	}
	if data, ok := rpc.ErrorDataOf(err); ok {
		switch {
		case data.Kind == rpc.ErrorUnknownOutcome:
			return outcomeUnknown
		case data.Kind == rpc.ErrorBusy && data.OperationID == id:
			return outcomeRunning
		}
	}
	if _, ok := errors.AsType[*rpc.RPCError](err); ok {
		return outcomeFailed
	}
	return outcomeUnknown
}

func (m *machine) succeed(mu *mutation, raw jsontext.Value) {
	m.mutations = slices.DeleteFunc(m.mutations, func(x *mutation) bool { return x == mu })
	if mu.method == methodThreadStart {
		m.threadStarted(mu, raw)
		return
	}
	ep, ts := m.thread(mu.key.thread())
	if ts == nil {
		return
	}
	if mu.method == methodTurnStart && ts.proj != nil {
		m.startedTurn(ts, raw)
	}
	m.touch(ts)
	m.progress(ep, ts)
}

func (m *machine) startedTurn(ts *threadState, raw jsontext.Value) {
	response, err := decode[turnNote](raw)
	if err != nil || response.Turn.ID == "" {
		return
	}
	if t, ok := ts.proj.turnByID[response.Turn.ID]; ok && t.final() {
		return
	}
	ts.starting = response.Turn.ID
}

func (m *machine) fail(mu *mutation, err error) {
	mu.state, mu.err = MutationFailed, err
	if _, ts := m.thread(mu.key.thread()); ts != nil {
		m.touch(ts)
	}
}

func (m *machine) abandonWaiting(ep *endpointState, cause error) {
	for _, mu := range m.mutations {
		if mu.endpoint() == ep.id && mu.state == MutationWaiting {
			m.fail(mu, cause)
		}
	}
}

func (m *machine) reconcile(ep *endpointState) {
	for _, mu := range m.mutations {
		if mu.endpoint() == ep.id && mu.state == MutationUnknown && mu.receipt {
			m.readReceipt(ep, mu)
		}
	}
}

func (m *machine) readReceipt(ep *endpointState, mu *mutation) {
	mu.state = MutationReconciling
	mu.ticket = m.call(ep, receiptCall{id: mu.id, link: ep.link}, methodOperationRead, operationParams{OperationID: mu.id})
}

func (c receiptCall) resume(m *machine, r resultMsg) {
	mu := m.mutation(c.id)
	if mu == nil || mu.ticket != r.ticket || mu.state != MutationReconciling {
		return
	}
	mu.state = MutationUnknown
	if r.err != nil {
		m.unreadReceipt(mu, r.err)
		return
	}
	record, err := decode[rpc.OperationRecord](r.raw)
	if err != nil {
		mu.err = err
		return
	}
	m.settleReceipt(mu, record)
}

func (m *machine) unreadReceipt(mu *mutation, err error) {
	if data, ok := rpc.ErrorDataOf(err); ok && data.Kind == rpc.ErrorNotFound {
		m.fail(mu, errNeverStarted)
		return
	}
	mu.err = err
}

func (m *machine) settleReceipt(mu *mutation, record rpc.OperationRecord) {
	switch record.State {
	case "succeeded":
		m.succeed(mu, record.Result)
	case "failed":
		m.fail(mu, recordError(record))
	default:
		mu.progress = record.State
	}
}

func recordError(record rpc.OperationRecord) error {
	if record.Error != nil {
		return record.Error
	}
	return errOperationFailed
}
