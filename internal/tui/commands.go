package tui

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"runtime/debug"
	"slices"
	"strconv"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

const (
	requestTimeout = 30 * time.Second
	maxFollowups   = 32
)

func (m *model) commandContext() (context.Context, context.CancelFunc) {
	ctx, cancel := context.WithTimeout(context.Background(), requestTimeout)
	if m.done == nil {
		return ctx, cancel
	}
	go func() {
		select {
		case <-m.done:
			cancel()
		case <-ctx.Done():
		}
	}()
	return ctx, cancel
}

func (m *model) call(op, method string, params, result any, text string) tea.Cmd {
	turnID := m.turnID
	return func() tea.Msg {
		if m.client == nil {
			return resultMsg{op: op, text: text, err: errors.New("rpc client is not connected")}
		}
		ctx, cancel := m.commandContext()
		defer cancel()
		err := m.client.Call(ctx, method, params, result)
		return resultMsg{op: op, value: result, text: text, turnID: turnID, err: err}
	}
}

func (m *model) initialize() tea.Cmd {
	return func() tea.Msg {
		if m.client == nil {
			return resultMsg{op: "initialize", err: errors.New("rpc client is not connected")}
		}
		ctx, cancel := m.commandContext()
		defer cancel()
		params := rpc.InitializeParams{ClientInfo: rpc.ClientInfo{Name: "dalgona", Title: "dalgona TUI", Version: clientVersion()}}
		var result rpc.InitializeResult
		if err := m.client.Call(ctx, "initialize", params, &result); err != nil {
			return resultMsg{op: "initialize", err: err}
		}
		return resultMsg{op: "initialize", err: m.client.Notify("initialized", nil)}
	}
}

func clientVersion() string {
	info, ok := debug.ReadBuildInfo()
	if !ok {
		return "(devel)"
	}
	return cmp.Or(info.Main.Version, "(devel)")
}

func (m *model) loadKeywords() tea.Cmd {
	var keywords []Keyword
	return m.call("keywords", "dalgona/keywords/list", rpc.DalgonaKeywordsListParams{}, &keywords, "")
}

func (m *model) startThread() tea.Cmd {
	if m.opts.ThreadID != "" {
		return m.resumeThread(m.opts.ThreadID)
	}
	return m.newThread()
}

func (m *model) newThread() tea.Cmd {
	params := rpc.ThreadStartParams{
		Model:          m.modelName,
		ApprovalPolicy: approvalPolicy(m.approvalMode),
		Sandbox:        sandboxPolicy(m.sandbox),
	}
	return m.call("new", "thread/start", params, new(rpc.ThreadStartResponse), "")
}

func approvalPolicy(mode string) string {
	switch mode {
	case "yolo":
		return "never"
	case "ask":
		return "untrusted"
	default:
		return "on-request"
	}
}

func sandboxPolicy(mode string) string {
	if mode == "off" {
		return "danger-full-access"
	}
	return "workspace-write"
}

func (m *model) submitPrompt() tea.Cmd {
	text := strings.TrimSpace(m.composer.Value())
	if text == "" {
		return m.startQueuedFollowup()
	}
	m.composer.SetValue("")
	if strings.HasPrefix(text, "/") {
		return m.executeSlash(text)
	}
	if m.threadID == "" {
		m.notice = "thread is not ready; wait for startup or use /new to retry"
		m.composer.SetValue(text)
		return nil
	}
	if m.state == stateRunning || m.state == stateAwaitingApproval {
		return m.sendSteer(text)
	}
	return m.sendPrompt(text)
}

func (m *model) sendPrompt(text string) tea.Cmd {
	m.setState(stateRunning)
	params := rpc.TurnStartParams{ThreadID: m.threadID, Input: textInput(text)}
	result := new(rpc.TurnStartResponse)
	return m.call("send", "turn/start", params, result, text)
}

func (m *model) sendSteer(text string) tea.Cmd {
	params := rpc.TurnSteerParams{ThreadID: m.threadID, ExpectedTurnID: m.turnID, Input: textInput(text)}
	return m.call("steer", "turn/steer", params, new(rpc.TurnSteerResponse), text)
}

func textInput(text string) []rpc.InputItem {
	return []rpc.InputItem{{Type: "text", Text: text}}
}

func (m *model) queueFollowup() tea.Cmd {
	text := strings.TrimSpace(m.composer.Value())
	if text == "" {
		return nil
	}
	if len(m.followups) >= maxFollowups {
		m.notice = fmt.Sprintf("follow-up queue is full (%d); wait for the active turn or press Esc", maxFollowups)
		return nil
	}
	m.followups = append(m.followups, text)
	m.composer.SetValue("")
	m.notice = fmt.Sprintf("queued follow-up %d of %d", len(m.followups), maxFollowups)
	if m.state == stateIdle {
		return m.startQueuedFollowup()
	}
	return nil
}

func (m *model) startQueuedFollowup() tea.Cmd {
	if len(m.followups) == 0 || m.state == stateRunning || m.state == stateAwaitingApproval {
		return nil
	}
	text := m.followups[0]
	m.followups = m.followups[1:]
	m.setState(stateRunning)
	params := rpc.TurnStartParams{ThreadID: m.threadID, Input: textInput(text)}
	result := new(rpc.TurnStartResponse)
	return m.call("followup", "turn/start", params, result, text)
}

func (m *model) interrupt() tea.Cmd {
	if m.selector != nil {
		m.selector = nil
		return nil
	}
	if m.state != stateRunning && m.state != stateAwaitingApproval {
		m.notice = "idle; nothing to interrupt"
		return nil
	}
	params := rpc.TurnInterruptParams{ThreadID: m.threadID, TurnID: m.turnID}
	return m.call("interrupt", "turn/interrupt", params, nil, "")
}

func (m *model) executeSlash(input string) tea.Cmd {
	name, argument, _ := strings.Cut(strings.TrimPrefix(input, "/"), " ")
	argument = strings.TrimSpace(argument)
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

func (m *model) executeThreadCommand(name string) tea.Cmd {
	switch name {
	case "new":
		return m.newThread()
	case "compact":
		params := rpc.ThreadCompactStartParams{ThreadID: m.threadID}
		return m.call("compact", "thread/compact/start", params, nil, "")
	case "tree":
		return m.treeCommand("tree")
	case "revert":
		return m.treeCommand("revertTree")
	case "fork":
		params := rpc.ThreadForkParams{ThreadID: m.threadID}
		result := new(rpc.ThreadForkResponse)
		return m.call("fork", "thread/fork", params, result, "")
	default:
		m.notice = "unknown command; use /model /resume /new /compact /login /tree /revert /fork /advisor"
		return nil
	}
}

func (m *model) advisorCommand(argument string) tea.Cmd {
	params := rpc.DalgonaAdvisorStatusParams{ThreadID: m.threadID}
	switch argument {
	case "":
		return m.call("advisorToggle", "dalgona/advisor/status", params, new([]rpc.AdvisorState), "")
	case "status":
		return m.call("advisorStatus", "dalgona/advisor/status", params, new([]rpc.AdvisorState), "")
	case "reset":
		return m.call("advisorReset", "dalgona/advisor/reset", params, nil, "")
	default:
		m.notice = "usage: /advisor to turn the advisor on or off, /advisor status, or /advisor reset"
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
	if argument != "" {
		return m.resumeThread(argument)
	}
	result := new(rpc.ThreadListResponse)
	return m.call("threads", "thread/list", rpc.ThreadListParams{Limit: 50}, result, "")
}

func (m *model) resumeThread(id string) tea.Cmd {
	return func() tea.Msg {
		if m.client == nil {
			return resultMsg{op: "resumed", err: errors.New("rpc client is not connected")}
		}
		ctx, cancel := m.commandContext()
		defer cancel()
		var resumed rpc.ThreadResumeResponse
		if err := m.client.Call(ctx, "thread/resume", rpc.ThreadResumeParams{ThreadID: id}, &resumed); err != nil {
			return resultMsg{op: "resumed", err: err}
		}
		var read rpc.ThreadReadResponse
		err := m.client.Call(ctx, "thread/read", rpc.ThreadReadParams{ThreadID: id, IncludeTurns: true}, &read)
		return resultMsg{op: "resumed", value: &read, err: err}
	}
}

func (m *model) loginCommand(provider string) tea.Cmd {
	if provider == "" {
		m.notice = "usage: /login <provider>; for example /login openai or /login chatgpt"
		return nil
	}
	params := rpc.AccountLoginStartParams{Provider: provider}
	result := new(rpc.LoginAccountResponse)
	return m.call("login", "account/login/start", params, result, provider)
}

func (m *model) treeCommand(op string) tea.Cmd {
	params := rpc.ThreadTreeParams{ThreadID: m.threadID}
	result := new(rpc.ThreadTreeResponse)
	return m.call(op, "dalgona/thread/tree", params, result, "")
}

func (m *model) selectKey(key string) tea.Cmd {
	if len(m.selector.choices) == 0 {
		m.selector = nil
		return nil
	}
	switch key {
	case "up", "ctrl+p":
		m.selector.cursor--
	case "down", "ctrl+n":
		m.selector.cursor++
	case "enter":
		return m.activateChoice()
	case "esc":
		m.selector = nil
	}
	if m.selector != nil {
		count := len(m.selector.choices)
		m.selector.cursor = (m.selector.cursor%count + count) % count
	}
	return nil
}

func (m *model) activateChoice() tea.Cmd {
	selected := m.selector.choices[m.selector.cursor]
	kind := m.selector.kind
	m.selector = nil
	switch kind {
	case "model":
		return m.setModel(selected.id)
	case "resume":
		return m.resumeThread(selected.id)
	case "tree":
		params := rpc.ThreadLeafSetParams{ThreadID: m.threadID, TurnID: selected.id}
		return m.call("leaf", "dalgona/thread/leaf/set", params, nil, "")
	case "revert":
		params := rpc.ThreadRevertParams{ThreadID: m.threadID, BeforeTurnID: selected.id}
		return m.call("revert", "thread/revert", params, nil, "")
	default:
		return nil
	}
}

func (m *model) applyResult(msg resultMsg) tea.Cmd {
	if strings.HasPrefix(msg.op, "advisor") {
		return m.applyAdvisorResult(msg)
	}
	if msg.err != nil {
		return m.applyResultError(msg)
	}
	switch msg.op {
	case "keywords", "models", "threads", "tree", "revertTree", "resumed", "new", "fork":
		return m.applyDataResult(msg)
	case "send", "followup":
		return m.applyTurnAccepted(msg.value)
	default:
		return m.applyConfirmation(msg)
	}
}

func (m *model) applyDataResult(msg resultMsg) tea.Cmd {
	switch msg.op {
	case "keywords":
		return m.applyKeywords(msg.value)
	case "models":
		return m.applyModels(msg.value)
	case "threads":
		return m.applyThreads(msg.value)
	case "tree", "revertTree":
		return m.applyTree(msg.op, msg.value)
	case "resumed":
		return m.applyResumed(msg.value)
	case "new":
		return m.applyNewThread(msg.value)
	case "fork":
		return m.applyForked(msg.value)
	default:
		return nil
	}
}

func (m *model) applyAdvisorResult(msg resultMsg) tea.Cmd {
	if msg.err != nil {
		m.notice = advisorFailure(msg.err)
		return nil
	}
	states, _ := msg.value.(*[]rpc.AdvisorState)
	switch msg.op {
	case "advisorToggle", "advisorStatus":
		if states == nil {
			return m.unexpectedResult("advisor status")
		}
		if msg.op == "advisorToggle" {
			return m.toggleAdvisor(*states)
		}
		m.notice = advisorStatusText(*states)
	case "advisorSet":
		m.notice = "advisor off; run /advisor to turn it back on"
		if msg.text == "true" {
			m.notice = "advisor on; it reviews after each turn"
		}
	case "advisorReset":
		m.notice = "advisor reset; it reviews the current branch at the next turn"
	}
	return nil
}

func advisorFailure(err error) string {
	if rpcErr, ok := errors.AsType[*rpc.RPCError](err); ok && rpcErr.Code == -32001 {
		return "advisor unavailable: " + rpcErr.Message + "; set [advisor] enabled = true in config.toml, then restart dalgona"
	}
	return fmt.Sprintf("advisor request failed: %v; retry the /advisor command", err)
}

func (m *model) toggleAdvisor(states []rpc.AdvisorState) tea.Cmd {
	enabled := !slices.ContainsFunc(states, func(state rpc.AdvisorState) bool { return state.Status != "disabled" })
	params := rpc.DalgonaAdvisorSetParams{ThreadID: m.threadID, Enabled: enabled}
	return m.call("advisorSet", "dalgona/advisor/set", params, nil, strconv.FormatBool(enabled))
}

func advisorStatusText(states []rpc.AdvisorState) string {
	if len(states) == 0 {
		return "no advisors configured; add [advisor] to config.toml"
	}
	lines := make([]string, 0, len(states))
	for _, state := range states {
		lines = append(lines, fmt.Sprintf("advisor %s · %s · %s · %d notes · %d in / %d out tokens",
			state.Name, state.Status, cmp.Or(state.Model, "default model"), state.Notes, state.Usage.InputTokens, state.Usage.OutputTokens))
	}
	return strings.Join(lines, "\n")
}

func (m *model) applyConfirmation(msg resultMsg) tea.Cmd {
	switch msg.op {
	case "interrupt":
		if msg.turnID != m.turnID {
			return nil
		}
		m.setState(stateInterrupted)
		m.notice = "interrupt requested; wait for turn completion before sending"
	case "compact":
		m.notice = "compaction started; wait for the context compaction item"
	case "revert":
		m.notice = "files changed by later turns are not reverted"
	case "leaf":
		m.notice = "active branch changed; files changed by later turns are not reverted"
	case "modelSet":
		m.modelName = msg.text
		m.notice = "model set to " + msg.text
	case "login":
		return m.applyLogin(msg.value)
	case "steer":
		m.notice = "steering message accepted"
	}
	return nil
}

func (m *model) applyResultError(msg resultMsg) tea.Cmd {
	if msg.op == "send" || msg.op == "steer" {
		m.restoreComposer(msg.text)
	}
	if msg.op == "followup" {
		m.followups = append([]string{msg.text}, m.followups...)
	}
	if msg.op == "send" || msg.op == "followup" {
		m.setState(stateFailed)
	}
	if msg.op == "keywords" {
		m.notice = fmt.Sprintf("keyword highlighting unavailable: %v; prompting is ready, restart to retry", msg.err)
		return nil
	}
	m.notice = fmt.Sprintf("%s failed: %v; retry, %s, or /new", msg.op, msg.err, m.resumeHint())
	return nil
}

func (m *model) restoreComposer(text string) {
	current := m.composer.Value()
	if current != "" {
		text += "\n" + current
	}
	m.composer.SetValue(text)
}

func (m *model) applyKeywords(value any) tea.Cmd {
	keywords, ok := value.(*[]Keyword)
	if !ok {
		m.notice = "keyword list has an unexpected shape; restart the TUI to retry"
		return nil
	}
	m.keywords = *keywords
	return nil
}

func (m *model) applyModels(value any) tea.Cmd {
	response, ok := value.(*rpc.ModelListResponse)
	if !ok {
		return m.unexpectedResult("models")
	}
	choices := make([]choice, 0, len(response.Data))
	for _, item := range response.Data {
		if item.Hidden {
			continue
		}
		choices = append(choices, choice{id: item.ID, label: item.ID, detail: item.DisplayName, active: item.ID == m.modelName})
	}
	if len(choices) == 0 {
		m.notice = "no models are available; run /login <provider> and retry /model"
		return nil
	}
	m.selector = &selector{kind: "model", title: "choose a model", choices: choices}
	return nil
}

func (m *model) applyThreads(value any) tea.Cmd {
	response, ok := value.(*rpc.ThreadListResponse)
	if !ok {
		return m.unexpectedResult("threads")
	}
	choices := make([]choice, 0, len(response.Data))
	for _, item := range response.Data {
		choices = append(choices, choice{id: item.ID, label: threadTitle(item), detail: threadState(item.Status), active: item.ID == m.threadID})
	}
	if len(choices) == 0 {
		m.notice = "no threads are available; use /new to start one"
		return nil
	}
	m.selector = &selector{kind: "resume", title: "resume a thread", choices: choices}
	return nil
}

func (m *model) applyTree(op string, value any) tea.Cmd {
	response, ok := value.(*rpc.ThreadTreeResponse)
	if !ok {
		return m.unexpectedResult("thread tree")
	}
	kind := "tree"
	title := "choose the active branch"
	choices := make([]choice, 0, len(response.Turns))
	for _, turn := range response.Turns {
		if op == "revertTree" {
			kind = "revert"
			title = "revert before a turn"
		}
		if op == "revertTree" && !turn.Active {
			continue
		}
		choices = append(choices, choice{id: turn.TurnID, label: turn.Preview, detail: turn.TurnID, active: turn.Active})
	}
	if len(choices) == 0 {
		m.notice = "no turns are available on this path; send a prompt first"
		return nil
	}
	m.selector = &selector{kind: kind, title: title, choices: choices}
	return nil
}

func (m *model) applyResumed(value any) tea.Cmd {
	response, ok := value.(*rpc.ThreadReadResponse)
	if !ok {
		return m.unexpectedResult("resumed thread")
	}
	m.setThread(response.Thread.ID)
	m.title = threadTitle(response.Thread)
	m.setState(threadState(response.Thread.Status))
	m.live = nil
	lines := make([]string, 0, len(response.Thread.Turns))
	for _, turn := range response.Thread.Turns {
		for _, item := range turn.Items {
			lines = append(lines, m.renderSettledItem(item))
		}
	}
	if len(lines) == 0 {
		return tea.Println("resumed " + m.title)
	}
	return tea.Println(strings.Join(lines, "\n"))
}

func (m *model) applyNewThread(value any) tea.Cmd {
	response, ok := value.(*rpc.ThreadStartResponse)
	if !ok {
		return m.unexpectedResult("new thread")
	}
	m.setThread(response.Thread.ID)
	m.title = threadTitle(response.Thread)
	m.live = nil
	m.setState(stateIdle)
	return tea.Println("started " + m.title)
}

func (m *model) applyForked(value any) tea.Cmd {
	response, ok := value.(*rpc.ThreadForkResponse)
	if !ok {
		return m.unexpectedResult("forked thread")
	}
	m.setThread(response.Thread.ID)
	return m.resumeThread(response.Thread.ID)
}

func (m *model) applyTurnAccepted(value any) tea.Cmd {
	response, ok := value.(*rpc.TurnStartResponse)
	if !ok {
		return m.unexpectedResult("accepted turn")
	}
	if _, done := m.completedTurns[response.Turn.ID]; done {
		return nil
	}
	m.turnID = response.Turn.ID
	m.setState(stateRunning)
	return nil
}

func (m *model) applyLogin(value any) tea.Cmd {
	response, ok := value.(*rpc.LoginAccountResponse)
	if !ok {
		return m.unexpectedResult("login")
	}
	url := cmp.Or(response.AuthURL, response.VerificationURL)
	if url == "" {
		m.notice = "login started; follow the provider instructions or run /login again to retry"
		return nil
	}
	m.notice = "login: open " + url + " in a browser; the TUI will resume when login completes"
	if response.UserCode != "" {
		m.notice += "; enter code " + response.UserCode
	}
	return nil
}

func (m *model) unexpectedResult(name string) tea.Cmd {
	m.notice = name + " response has an unexpected shape; reconnect and retry"
	return nil
}
