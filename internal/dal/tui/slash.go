package tui

import (
	"errors"
	"fmt"
	"math"
	"slices"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func (m *model) executeSlash(name, argument string) tea.Cmd {
	if cmd, handled := m.machineSlashCommand(name, argument); handled {
		return cmd
	}
	if m.machineOffline() && name != "help" {
		return nil
	}
	if cmd, handled := m.slashPanelCommand(name, argument); handled {
		return cmd
	}
	switch name {
	case "model":
		return m.modelCommand(argument)
	case "resume":
		return m.resumeCommand(argument)
	default:
		return m.executeThreadCommand(name)
	}
}

func (m *model) machineSlashCommand(name, argument string) (tea.Cmd, bool) {
	if name != "machines" {
		return nil, false
	}
	action := parseMachineAction(argument)
	if action.id == "" && (action.verb == "connect" || action.verb == "disconnect") {
		action.id = m.conn.link.Endpoint
	}
	return func() tea.Msg { return action }, true
}

func (m *model) slashPanelCommand(name, argument string) (tea.Cmd, bool) {
	switch name {
	case "login":
		return m.openLogin(argument), true
	case "advisor":
		return m.advisorCommand(argument), true
	case "settings":
		m.openSettings()
		return nil, true
	case "help":
		m.openHelp()
		return nil, true
	default:
		return nil, false
	}
}

var busyThreadCommands = map[string]bool{"new": true, "resume": true, "compact": true, "tree": true, "revert": true, "fork": true, "review": true, confirmRewind: true}

func (m *model) executeThreadCommand(name string) tea.Cmd {
	if busyThreadCommands[name] && m.busyRefusal(name) {
		return nil
	}
	switch name {
	case "new":
		m.openNewConfirm()
	case "compact":
		m.openCompactConfirm()
	case "tree":
		return m.treeCommand("tree")
	case "revert":
		return m.treeCommand("revertTree")
	case "fork":
		return m.treeCommand("forkTree")
	case "review":
		m.openReviewPanel()
	}
	return nil
}

func (m *model) openNewConfirm() {
	c := confirmation{kind: confirmNew, title: "Start a new thread?", action: "[enter] start  [esc] cancel"}
	if n := len(m.queue); n > 0 {
		c.lines = []string{fmt.Sprintf("%d follow-ups move to it", n)}
	}
	m.openConfirm(c)
}

func (m *model) openCompactConfirm() {
	c := confirmation{kind: confirmCompact, title: "Compact the context?", action: "[enter] compact  [esc] cancel"}
	if fraction, ok := contextFraction(m.thread.usage); ok {
		c.lines = []string{fmt.Sprintf("ctx %d%% of %d tokens", int(math.Round(fraction*100)), m.thread.usage.ModelContextWindow)}
	}
	m.openConfirm(c)
}

func (m *model) openReviewPanel() {
	m.openList(&listPanel{kind: "review", title: "Review changes", efforts: map[string]effortPick{}, items: []listItem{
		{id: reviewUncommitted, label: "Uncommitted changes"},
		{id: reviewBase, label: "Base branch"},
		{id: reviewCommit, label: "Commit"},
		{id: reviewCustom, label: "Custom"},
	}})
}

const (
	reviewUncommitted = "uncommittedChanges"
	reviewBase        = "baseBranch"
	reviewCommit      = "commit"
	reviewCustom      = "custom"
)

func (m *model) reviewStart(target, text string) tea.Cmd {
	if m.busyRefusal("review") {
		return nil
	}
	params := rpc.ReviewStartParams{ThreadID: m.thread.id, Target: rpc.ReviewTarget{Type: target}}
	switch target {
	case reviewBase:
		params.Target.Branch = text
	case reviewCommit:
		params.Target.SHA = text
	case reviewCustom:
		params.Target.Instructions = text
	}
	return m.call("review", "review/start", params, new(rpc.ReviewStartResponse), "")
}

func (m *model) advisorCommand(argument string) tea.Cmd {
	params := rpc.DalgonaAdvisorStatusParams{ThreadID: m.thread.id}
	switch argument {
	case "":
		return m.listCall("advisorPanel", "dalgona/advisor/status", params, new([]rpc.AdvisorState))
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
	return m.listCall("models", "model/list", rpc.ModelListParams{}, result)
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
	return m.listCall("threads", "thread/list", rpc.ThreadListParams{Limit: 50}, result)
}

func (m *model) resumeThread(id string) tea.Cmd {
	target := ""
	if id != m.thread.id || (m.conn.link.Endpoint != "" && !m.conn.threadOpen) {
		target = id
	}
	seq := m.resetPendingResume(target)
	revision := m.thread.turnRevision
	client, done := m.conn.client, m.conn.done
	return m.conn.link.scope(func() tea.Msg {
		if client == nil {
			return resultMsg{op: "resumed", seq: seq, err: errors.New("rpc client is not connected")}
		}
		ctx, cancel := requestContext(done)
		defer cancel()
		var resumed rpc.ThreadResumeResponse
		if err := client.Call(ctx, "thread/resume", rpc.ThreadResumeParams{ThreadID: id}, &resumed); err != nil {
			return resultMsg{op: "resumed", seq: seq, err: err}
		}
		return resultMsg{op: "resumed", seq: seq, value: &resumedThread{
			ThreadReadResponse: rpc.ThreadReadResponse{Thread: resumed.Thread},
			effort:             resumed.ReasoningEffort, turnEffort: resumed.TurnEffort, turnRevision: &revision,
			approval: resumed.ApprovalPolicy, model: resumed.Model, cwd: resumed.Cwd, resumeSeq: seq,
		}}
	})
}

func (m *model) callSeq(seq int, op, method string, params, result any) tea.Cmd {
	return m.callRPC(method, params, resultMsg{op: op, seq: seq, value: result})
}

func (m *model) listCall(op, method string, params, result any) tea.Cmd {
	m.listSeq++
	return m.callSeq(m.listSeq, op, method, params, result)
}

func (m *model) treeCommand(op string) tea.Cmd {
	params := rpc.ThreadTreeParams{ThreadID: m.thread.id}
	result := new(rpc.ThreadTreeResponse)
	return m.listCall(op, "dalgona/thread/tree", params, result)
}

func (m *model) busyRefusal(name string) bool {
	word := busyWord(m.thread.state)
	if word == "" {
		return false
	}
	label := "/" + name
	if name == confirmRewind {
		label = name
	}
	m.noticef("%s is unavailable while a turn is %s; wait for it to finish or interrupt it first", label, word)
	return true
}

func busyWord(state string) string {
	switch state {
	case stateRunning:
		return "working"
	case stateAwaitingApproval:
		return "waiting for approval"
	case stateAwaitingInput:
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
	{name: "resume", args: "[id]", summary: "Search earlier conversations, or open a thread by id"},
	{name: "machines", args: "[action]", summary: "Browse, connect, select, or add machines"},
	{name: "new", summary: "Start a new thread"},
	{name: "compact", summary: "Compact the context of this thread"},
	{name: "login", args: "[provider]", summary: "Sign in to a provider"},
	{name: "tree", summary: "Show the turns of this thread and move to one"},
	{name: "revert", summary: "Revert this thread to before a turn"},
	{name: "fork", summary: "Fork this thread at a turn you pick"},
	{name: "advisor", args: "[status|reset]", summary: "Turn the advisor on or off, show its status, or reset it"},
	{name: "review", summary: "Review uncommitted changes, a branch, or a commit"},
	{name: "settings", summary: "Show settings"},
	{name: "help", summary: "Show keys"},
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
		return name, arg, slashCommand
	default:
		return name, "", slashUnknown
	}
}

func (c editor) slashParts() (name, arg string, kind slashKind) {
	start := c.slashStart()
	end := c.slashEnd(start)
	word := string(c.text[start:end])
	name, _, kind = parseSlash(word)
	switch kind {
	case slashCommand:
		if end < len(c.text) && c.text[end] == ' ' && !c.hasChipsAt(end) {
			end++
		}
		return name, c.expandRange(end, len(c.text)), kind
	case slashPrompt:
		if strings.HasPrefix(word, "//") {
			return "", string(c.text[:start]) + c.expandRange(start+1, len(c.text)), kind
		}
		return "", c.Expanded(), kind
	case slashUnknown:
		return name, "", kind
	}
	return name, "", kind
}

func (c editor) slashStart() int {
	i := 0
	for i < len(c.text) && c.text[i] == ' ' && !c.hasChipsAt(i) {
		i++
	}
	return i
}
