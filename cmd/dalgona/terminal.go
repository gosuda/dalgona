package main

import (
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/vtstate"
)

func newTerminalHost() *rpcserver.TerminalHost {
	return rpcserver.NewTerminalHost(rpcserver.TerminalEngine{
		Version:       vtstate.Version,
		EngineVersion: vtstate.EngineVersion(),
		New:           func(columns, rows int) rpcserver.TerminalScreen { return vtstate.New(columns, rows) },
	})
}
