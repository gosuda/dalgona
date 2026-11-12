package tui

import (
	"errors"
	"slices"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func (m *model) executeSlash(name, argument string) tea.Cmd {
	switch name {
	case "model":
		return m.modelCommand(argument)
	case "resume":
		return m.resumeCommand(argument)
	case "login":
		return m.loginCommand(argument)
	case "advisor":
		return m.advisorCommand(argument)
	default:
		return m.executeThreadCommand(name)
	}
}

var busyThreadCommands = map[string]bool{"new": true, "compact": true, "tree": true, "revert": true, "fork": true}

func (m *model) executeThreadCommand(name string) tea.Cmd {
	if busyThreadCommands[name] && m.busyRefusal(name) {
		return nil
	}
	switch name {
	case "new":
		return m.newThread()
	case "compact":
		params := rpc.ThreadCompactStartParams{ThreadID: m.thread.id}
		return m.call("compact", "thread/compact/start", params, nil, "")
	case "tree":
		return m.treeCommand("tree")
	case "revert":
		return m.treeCommand("revertTree")
	case "fork":
		params := rpc.ThreadForkParams{ThreadID: m.thread.id}
		result := new(rpc.ThreadForkResponse)
		return m.call("fork", "thread/fork", params, result, "")
	default:
		return nil
	}
}

func (m *model) advisorCommand(argument string) tea.Cmd {
	params := rpc.DalgonaAdvisorStatusParams{ThreadID: m.thread.id}
	switch argument {
	case "":
		return m.call("advisorToggle", "dalgona/advisor/status", params, new([]rpc.AdvisorState), "")
	case "status":
		return m.call("advisorStatus", "dalgona/advisor/status", params, new([]rpc.AdvisorState), "")
	case "reset":
		return m.call("advisorReset", "dalgona/advisor/reset", params, nil, "")
	default:
		m.noticef("usage: /advisor to turn the advisor on or off, /advisor status, or /advisor reset")
		return nil
	}
}

func (m *model) modelCommand(argument string) tea.Cmd {
	if argument != "" {
		return m.setModel(argument)
	}
	result := new(rpc.ModelListResponse)
	return m.call("models", "model/list", rpc.ModelListParams{}, result, "")
}

func (m *model) setModel(id string) tea.Cmd {
	params := rpc.ConfigValueWriteParams{KeyPath: "model", MergeStrategy: "replace", Value: id}
	return m.call("modelSet", "config/value/write", params, nil, id)
}

func (m *model) resumeCommand(argument string) tea.Cmd {
	if m.busyRefusal("resume") {
		return nil
	}
	if argument != "" {
		return m.resumeThread(argument)
	}
	result := new(rpc.ThreadListResponse)
	return m.call("threads", "thread/list", rpc.ThreadListParams{Limit: 50}, result, "")
}

func (m *model) resumeThread(id string) tea.Cmd {
	return func() tea.Msg {
		if m.conn.client == nil {
			return resultMsg{op: "resumed", err: errors.New("rpc client is not connected")}
		}
		ctx, cancel := m.commandContext()
		defer cancel()
		var resumed rpc.ThreadResumeResponse
		if err := m.conn.client.Call(ctx, "thread/resume", rpc.ThreadResumeParams{ThreadID: id}, &resumed); err != nil {
			return resultMsg{op: "resumed", err: err}
		}
		var read rpc.ThreadReadResponse
		err := m.conn.client.Call(ctx, "thread/read", rpc.ThreadReadParams{ThreadID: id, IncludeTurns: true}, &read)
		return resultMsg{op: "resumed", value: &resumedThread{ThreadReadResponse: read, effort: resumed.ReasoningEffort}, err: err}
	}
}

func (m *model) loginCommand(provider string) tea.Cmd {
	if provider == "" {
		m.noticef("usage: /login <provider>; for example /login openai or /login chatgpt")
		return nil
	}
	params := rpc.AccountLoginStartParams{Provider: provider}
	result := new(rpc.LoginAccountResponse)
	return m.call("login", "account/login/start", params, result, provider)
}

func (m *model) treeCommand(op string) tea.Cmd {
	params := rpc.ThreadTreeParams{ThreadID: m.thread.id}
	result := new(rpc.ThreadTreeResponse)
	return m.call(op, "dalgona/thread/tree", params, result, "")
}

func (m *model) busyRefusal(name string) bool {
	word := busyWord(m.thread.state)
	if word == "" {
		return false
	}
	m.noticef("/%s is unavailable while a turn is %s; wait for it to finish or interrupt it first", name, word)
	return true
}

func busyWord(state string) string {
	switch state {
	case stateRunning:
		return "working"
	case stateAwaitingApproval:
		return "waiting for approval"
	case "awaiting input":
		return "waiting for an answer"
	default:
		return ""
	}
}

type command struct {
	name    string
	args    string
	summary string
}

var commands = []command{
	{name: "model", args: "[id]", summary: "Choose a model, or set one by id"},
	{name: "resume", args: "[id]", summary: "Open an earlier thread, or pick one from the list"},
	{name: "new", summary: "Start a new thread"},
	{name: "compact", summary: "Compact the context of this thread"},
	{name: "login", args: "[provider]", summary: "Sign in to a provider"},
	{name: "tree", summary: "Show the turns of this thread and move to one"},
	{name: "revert", summary: "Revert this thread to before a turn"},
	{name: "fork", summary: "Fork this thread at the last turn"},
	{name: "advisor", args: "[status|reset]", summary: "Turn the advisor on or off, show its status, or reset it"},
}

type slashKind int

const (
	slashCommand slashKind = iota
	slashPrompt
	slashUnknown
)

func parseSlash(text string) (name, arg string, kind slashKind) {
	if prompt, ok := strings.CutPrefix(text, "//"); ok {
		return "", "/" + prompt, slashPrompt
	}
	name, arg, _ = strings.Cut(strings.TrimPrefix(text, "/"), " ")
	switch {
	case strings.Contains(name, "/"):
		return "", text, slashPrompt
	case slices.ContainsFunc(commands, func(c command) bool { return c.name == name }):
		return name, strings.TrimSpace(arg), slashCommand
	default:
		return name, "", slashUnknown
	}
}
