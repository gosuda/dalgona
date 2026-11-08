package rpc

import (
	"errors"

	"github.com/gosuda/dalgona/internal/agent"
)

const (
	codeInvalidRequest = -32600
	codeMethodNotFound = -32601
	codeInvalidParams  = -32602
	codeInternal       = -32603
	codeNotAvailable   = -32001
)

func invalidParams(message string) error {
	return &RPCError{Code: codeInvalidParams, Message: message}
}

func invalidRequest(message string) error {
	return &RPCError{Code: codeInvalidRequest, Message: message}
}

func internalError(err error) error {
	if coded, ok := errors.AsType[*RPCError](err); ok {
		return coded
	}
	return &RPCError{Code: codeInternal, Message: err.Error()}
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
	case errors.Is(err, agent.ErrNothingToSummarize):
		return invalidRequest(err.Error())
	case errors.Is(err, agent.ErrDamaged), errors.Is(err, agent.ErrClosed), errors.Is(err, agent.ErrArchived):
		return invalidRequest(err.Error())
	}
	return internalError(err)
}
