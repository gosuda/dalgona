package state

import (
	"encoding/json/jsontext"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

const (
	metadataDeadline = 10 * time.Second
	pageDeadline     = 30 * time.Second
	leaseInterval    = 4 * time.Minute
	workerLimit      = 16
)

type effect interface{ run(o *Owner) }

type (
	callEffect struct {
		ticket   uint64
		link     client.Link
		method   string
		params   any
		deadline time.Duration
	}
	operationEffect struct {
		ticket   uint64
		link     client.Link
		op       rpc.Operation
		params   any
		deadline time.Duration
	}
	readEffect struct {
		ticket   uint64
		link     client.Link
		method   string
		params   func(offset string) any
		summary  rpc.ContentSummary
		deadline time.Duration
	}
	replyEffect struct {
		to    chan<- reply
		reply reply
	}
	respondEffect struct {
		link    client.Link
		thread  ThreadKey
		request string
		result  jsontext.Value
	}
	cancelEffect  struct{ ticket uint64 }
	connectEffect struct{ endpoint client.EndpointID }
	resyncEffect  struct{ endpoint client.EndpointID }
	attachEffect  struct{ endpoint client.EndpointID }
	detachEffect  struct{ detach func() }
	retireEffect  struct{ link client.Link }
	timerEffect   struct {
		after time.Duration
		fire  message
	}
)

type message interface{ apply(m *machine) }

type (
	seedMsg   struct{ endpoints []client.EndpointStatus }
	phaseMsg  struct{ change client.PhaseChange }
	notifyMsg struct {
		link   client.Link
		method string
		params jsontext.Value
		at     time.Time
	}
	arrivalMsg struct {
		link   client.Link
		params jsontext.Value
	}
	awaitMsg struct {
		link    client.Link
		request string
		waiter  chan<- reply
	}
	respondedMsg struct {
		thread  ThreadKey
		request string
		err     error
	}
	resultMsg struct {
		ticket   uint64
		endpoint client.EndpointID
		raw      jsontext.Value
		data     []byte
		err      error
	}
	leaseMsg struct {
		thread  ThreadKey
		history string
	}
	connectFailedMsg struct {
		endpoint client.EndpointID
		err      error
	}
	attachedMsg struct {
		endpoint client.EndpointID
		detach   func()
		err      error
	}
)

func (msg seedMsg) apply(m *machine)          { m.seed(msg.endpoints) }
func (msg phaseMsg) apply(m *machine)         { m.phase(msg.change) }
func (msg notifyMsg) apply(m *machine)        { m.notified(msg) }
func (msg arrivalMsg) apply(m *machine)       { m.arrived(msg) }
func (msg awaitMsg) apply(m *machine)         { m.awaited(msg) }
func (msg respondedMsg) apply(m *machine)     { m.responded(msg) }
func (msg resultMsg) apply(m *machine)        { m.resume(msg) }
func (msg leaseMsg) apply(m *machine)         { m.renewLease(msg) }
func (msg connectFailedMsg) apply(m *machine) { m.connectFailed(msg) }
func (msg attachedMsg) apply(m *machine)      { m.attached(msg) }
