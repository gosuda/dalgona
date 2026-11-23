package rpcserver

import (
	"errors"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
)

func invalidParams(message string) error {
	return &rpc.RPCError{Code: rpc.CodeInvalidParams, Message: message}
}

func invalidRequest(message string) error {
	return &rpc.RPCError{Code: rpc.CodeInvalidRequest, Message: message}
}

func internalError(err error) error {
	if coded, ok := errors.AsType[*rpc.RPCError](err); ok {
		return coded
	}
	return &rpc.RPCError{Code: rpc.CodeInternal, Message: err.Error()}
}

func unknownThread(id string) error { return invalidParams("unknown thread " + id) }

func hostError(err error, threadID, turnID string) error {
	switch {
	case errors.Is(err, agent.ErrThreadRunning):
		return invalidRequest("thread " + threadID + " is running")
	case errors.Is(err, agent.ErrTurnNotOnPath):
		return invalidParams("turn " + turnID + " is not on the active path")
	case errors.Is(err, agent.ErrUnknownTurn):
		return invalidParams("unknown turn " + turnID)
	case errors.Is(err, agent.ErrTurnNotRunning):
		return invalidRequest("turn " + turnID + " is not running")
	case errors.Is(err, agent.ErrNothingToSummarize):
		return invalidRequest(err.Error())
	case errors.Is(err, agent.ErrDamaged), errors.Is(err, agent.ErrClosed), errors.Is(err, agent.ErrArchived):
		return invalidRequest(err.Error())
	}
	return internalError(err)
}
