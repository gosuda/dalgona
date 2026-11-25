package state

import (
	"encoding/json/jsontext"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

type reply struct {
	result jsontext.Value
	err    error
}

type responder struct {
	waiter   chan<- reply
	held     *reply
	answered bool
	spent    bool
}

func (m *machine) answer(to *responder, r reply) {
	to.answered = true
	if to.waiter == nil {
		to.held = &r
		return
	}
	m.emit(replyEffect{to: to.waiter, reply: r})
	to.waiter, to.spent = nil, true
}

func (m *machine) awaited(msg awaitMsg) {
	to := m.waitingResponder(msg.link, msg.request)
	switch {
	case to == nil:
		m.emit(replyEffect{to: msg.waiter, reply: reply{err: rpc.NativeKind(rpc.ErrorUnavailable)}})
	case to.held != nil:
		m.emit(replyEffect{to: msg.waiter, reply: *to.held})
		to.held, to.spent = nil, true
	default:
		to.waiter = msg.waiter
	}
}

func (m *machine) waitingResponder(link client.Link, request string) *responder {
	ep, ok := m.endpoints[link.Endpoint]
	if !ok || !ep.live || ep.link != link {
		return nil
	}
	to := ep.responders[request]
	if to == nil || to.waiter != nil || to.spent {
		return nil
	}
	return to
}

func (m *machine) abandonResponders(ep *endpointState) {
	for _, to := range ep.responders {
		if to.waiter != nil {
			m.emit(replyEffect{to: to.waiter, reply: reply{err: rpc.NativeKind(rpc.ErrorUnavailable)}})
		}
	}
	ep.responders = map[string]*responder{}
}
