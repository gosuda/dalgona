package tui

import (
	"cmp"
	"context"
	"encoding/json/jsontext"

	tea "charm.land/bubbletea/v2"
	"charm.land/glamour/v2"
)

const (
	stateIdle             = "idle"
	stateRunning          = "running"
	stateAwaitingApproval = "awaiting approval"
	stateInterrupted      = "interrupted"
	stateFailed           = "failed"
	stateDisconnected     = "disconnected"
)

type caller interface {
	Call(context.Context, string, any, any) error
	Notify(string, any) error
}

type model struct {
	conn           connInfo
	thread         threadInfo
	live           liveState
	input          inputState
	screen         screenState
	out            outState
	notice         string
	queue          []string
	ctrlCArmed     bool
	pendingCommand tea.Cmd
	approvals      []approval
	selector       *selector
}

type connInfo struct {
	client caller
	opts   Options
	done   <-chan struct{}
}

type threadInfo struct {
	id             string
	turnID         string
	state          string
	seq            uint64
	completedTurns map[string]struct{}
	title          string
	modelName      string
	approvalMode   string
	sandbox        string
}

type liveState struct {
	items  []liveItem
	deltas *deltaDecoder
}

type inputState struct {
	composer      composer
	keywords      []Keyword
	phase         float64
	shimmerActive bool
}

type screenState struct {
	width   int
	focused bool
}

type outState struct {
	markdown map[int]*glamour.TermRenderer
}
type notificationMsg struct {
	method string
	params jsontext.Value
}

type serverRequestMsg struct {
	method string
	params jsontext.Value
	answer chan requestAnswer
}

type requestAnswer struct {
	value any
	err   error
}

type resultMsg struct {
	op     string
	value  any
	text   string
	turnID string
	err    error
}

type shimmerMsg struct{}

type disconnectedMsg struct {
	err error
}

func newModel(client caller, opts Options, done <-chan struct{}) model {
	approvalMode := cmp.Or(opts.Approval, "auto")
	sandbox := cmp.Or(opts.Sandbox, "workspace-write")
	if approvalMode == "yolo" {
		sandbox = "off"
	}
	return model{
		conn:   connInfo{client: client, opts: opts, done: done},
		input:  inputState{composer: newComposer()},
		screen: screenState{width: 80, focused: true},
		out:    outState{markdown: make(map[int]*glamour.TermRenderer)},
		live:   liveState{deltas: new(deltaDecoder)},
		thread: threadInfo{state: stateIdle, title: "new thread", modelName: opts.Model, approvalMode: approvalMode, sandbox: sandbox},
	}
}
