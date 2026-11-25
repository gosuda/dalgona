package state

import (
	"context"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

type Port interface {
	Endpoints() []client.EndpointStatus
	Connect(endpoint client.EndpointID) error
	Disconnect(endpoint client.EndpointID) error
	Attach(endpoint client.EndpointID) (detach func(), err error)
	Call(ctx context.Context, link client.Link, method string, params, result any) error
	CallOperation(ctx context.Context, link client.Link, op rpc.Operation, params, result any) error
	Respond(ctx context.Context, link client.Link, requestID string, result any) error
}

var (
	_ Port            = (*client.EndpointSet)(nil)
	_ client.Observer = (*Owner)(nil)
)

func lostPhase(p client.Phase) bool {
	switch p {
	case client.PhaseReconnecting, client.PhaseOffline, client.PhaseIncompatible, client.PhaseFailed:
		return true
	case client.PhaseIdle, client.PhaseConnecting, client.PhaseConnected:
		return false
	}
	return false
}

func refusesSend(p client.Phase) bool {
	switch p {
	case client.PhaseOffline, client.PhaseIncompatible, client.PhaseFailed:
		return true
	case client.PhaseIdle, client.PhaseConnecting, client.PhaseConnected, client.PhaseReconnecting:
		return false
	}
	return false
}
