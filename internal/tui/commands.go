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
	if m.conn.done == nil {
		return ctx, cancel
	}
	go func() {
		select {
		case <-m.conn.done:
			cancel()
		case <-ctx.Done():
		}
	}()
	return ctx, cancel
}

func (m *model) call(op, method string, params, result any, text string) tea.Cmd {
	turnID := m.thread.turnID
	return func() tea.Msg {
		if m.conn.client == nil {
			return resultMsg{op: op, text: text, err: errors.New("rpc client is not connected")}
		}
		ctx, cancel := m.commandContext()
		defer cancel()
		err := m.conn.client.Call(ctx, method, params, result)
		return resultMsg{op: op, value: result, text: text, turnID: turnID, err: err}
	}
}

func (m *model) initialize() tea.Cmd {
	return func() tea.Msg {
		if m.conn.client == nil {
			return resultMsg{op: "initialize", err: errors.New("rpc client is not connected")}
		}
		ctx, cancel := m.commandContext()
		defer cancel()
		params := rpc.InitializeParams{ClientInfo: rpc.ClientInfo{Name: "dalgona", Title: "dalgona TUI", Version: clientVersion()}}
		var result rpc.InitializeResult
		if err := m.conn.client.Call(ctx, "initialize", params, &result); err != nil {
			return resultMsg{op: "initialize", err: err}
		}
		return resultMsg{op: "initialize", err: m.conn.client.Notify("initialized", nil)}
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
	if m.conn.opts.ThreadID != "" {
		return m.resumeThread(m.conn.opts.ThreadID)
	}
	return m.newThread()
}

func (m *model) newThread() tea.Cmd {
	params := rpc.ThreadStartParams{
		Model:          m.thread.modelName,
		ApprovalPolicy: approvalPolicy(m.thread.approvalMode),
		Sandbox:        sandboxPolicy(m.thread.sandbox),
		Cwd:            m.conn.opts.Cwd,
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
	text := strings.TrimSpace(m.input.composer.Value())
	if text == "" {
		return m.startQueuedFollowup()
	}
	m.input.composer.SetValue("")
	if strings.HasPrefix(text, "/") {
		return m.executeSlash(text)
	}
	if m.thread.id == "" {
		m.noticef("thread is not ready; wait for startup or use /new to retry")
		m.input.composer.SetValue(text)
		return nil
	}
	if m.thread.state == stateRunning || m.thread.state == stateAwaitingApproval {
		return m.sendSteer(text)
	}
	return m.sendPrompt(text)
}

func (m *model) sendPrompt(text string) tea.Cmd {
	m.setState(stateRunning)
	params := rpc.TurnStartParams{ThreadID: m.thread.id, Input: textInput(text)}
	result := new(rpc.TurnStartResponse)
	return m.call("send", "turn/start", params, result, text)
}

func (m *model) sendSteer(text string) tea.Cmd {
	params := rpc.TurnSteerParams{ThreadID: m.thread.id, ExpectedTurnID: m.thread.turnID, Input: textInput(text)}
	return m.call("steer", "turn/steer", params, new(rpc.TurnSteerResponse), text)
}

func textInput(text string) []rpc.InputItem {
	return []rpc.InputItem{{Type: "text", Text: text}}
}

func (m *model) queueFollowup() tea.Cmd {
	text := strings.TrimSpace(m.input.composer.Value())
	if text == "" {
		return nil
	}
	if len(m.queue) >= maxFollowups {
		m.noticef("follow-up queue is full (%d); wait for the active turn or press Esc", maxFollowups)
		return nil
	}
	m.queue = append(m.queue, text)
	m.input.composer.SetValue("")
	m.noticef("queued follow-up %d of %d", len(m.queue), maxFollowups)
	if m.thread.state == stateIdle {
		return m.startQueuedFollowup()
	}
	return nil
}

func (m *model) startQueuedFollowup() tea.Cmd {
	if len(m.queue) == 0 || m.thread.state == stateRunning || m.thread.state == stateAwaitingApproval {
		return nil
	}
	text := m.queue[0]
	m.queue = m.queue[1:]
	m.setState(stateRunning)
	params := rpc.TurnStartParams{ThreadID: m.thread.id, Input: textInput(text)}
	result := new(rpc.TurnStartResponse)
	return m.call("followup", "turn/start", params, result, text)
}

func (m *model) interrupt() tea.Cmd {
	if m.selector != nil {
		m.selector = nil
		return nil
	}
	if m.thread.state != stateRunning && m.thread.state != stateAwaitingApproval {
		m.noticef("idle; nothing to interrupt")
		return nil
	}
	params := rpc.TurnInterruptParams{ThreadID: m.thread.id, TurnID: m.thread.turnID}
	return m.call("interrupt", "turn/interrupt", params, nil, "")
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
		m.errorf("%s", advisorFailure(msg.err))
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
		m.noticef("%s", advisorStatusText(*states))
	case "advisorSet":
		m.noticef("advisor off; run /advisor to turn it back on")
		if msg.text == "true" {
			m.noticef("advisor on; it reviews after each turn")
		}
	case "advisorReset":
		m.noticef("advisor reset; it reviews the current branch at the next turn")
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
	params := rpc.DalgonaAdvisorSetParams{ThreadID: m.thread.id, Enabled: enabled}
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
		if msg.turnID != m.thread.turnID {
			return nil
		}
		m.setState(stateInterrupted)
		m.noticef("interrupt requested; wait for turn completion before sending")
	case "compact":
		m.noticef("compaction started; wait for the context compaction item")
	case "revert":
		m.noticef("files changed by later turns are not reverted")
	case "leaf":
		m.noticef("active branch changed; files changed by later turns are not reverted")
	case "modelSet":
		m.thread.modelName = msg.text
		m.noticef("%s", "model set to "+msg.text)
	case "login":
		return m.applyLogin(msg.value)
	case "steer":
		m.noticef("steering message accepted")
	}
	return nil
}

func (m *model) applyResultError(msg resultMsg) tea.Cmd {
	if msg.op == "send" || msg.op == "steer" {
		m.restoreComposer(msg.text)
	}
	if msg.op == "followup" {
		m.queue = append([]string{msg.text}, m.queue...)
	}
	if msg.op == "send" || msg.op == "followup" {
		m.setState(stateFailed)
	}
	if msg.op == "keywords" {
		m.errorf("keyword highlighting unavailable: %v; prompting is ready, restart to retry", msg.err)
		return nil
	}
	m.errorf("%s failed: %v; retry, %s, or /new", msg.op, msg.err, m.resumeHint())
	return nil
}

func (m *model) restoreComposer(text string) {
	current := m.input.composer.Value()
	if current != "" {
		text += "\n" + current
	}
	m.input.composer.SetValue(text)
}

func (m *model) applyKeywords(value any) tea.Cmd {
	keywords, ok := value.(*[]Keyword)
	if !ok {
		m.errorf("keyword list has an unexpected shape; restart the TUI to retry")
		return nil
	}
	m.input.keywords = *keywords
	return nil
}

func (m *model) applyResumed(value any) tea.Cmd {
	response, ok := value.(*rpc.ThreadReadResponse)
	if !ok {
		return m.unexpectedResult("resumed thread")
	}
	m.setThread(response.Thread.ID)
	m.thread.title = threadTitle(response.Thread)
	m.setState(threadState(response.Thread.Status))
	m.live.items = nil
	lines := make([]string, 0, len(response.Thread.Turns))
	for _, turn := range response.Thread.Turns {
		for _, item := range turn.Items {
			lines = append(lines, m.renderSettledItem(item))
		}
	}
	if len(lines) == 0 {
		return tea.Println("resumed " + m.thread.title)
	}
	return tea.Println(strings.Join(lines, "\n"))
}

func (m *model) applyNewThread(value any) tea.Cmd {
	response, ok := value.(*rpc.ThreadStartResponse)
	if !ok {
		return m.unexpectedResult("new thread")
	}
	m.setThread(response.Thread.ID)
	m.thread.title = threadTitle(response.Thread)
	m.thread.sandbox = serverSandbox(response.Sandbox.Type, m.thread.sandbox)
	m.live.items = nil
	m.setState(stateIdle)
	return tea.Println("started " + m.thread.title)
}

func serverSandbox(serverType, fallback string) string {
	switch serverType {
	case "dangerFullAccess":
		return "off"
	case "workspaceWrite":
		return "workspace-write"
	case "readOnly":
		return "workspace-write"
	default:
		return fallback
	}
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
	if _, done := m.thread.completedTurns[response.Turn.ID]; done {
		return nil
	}
	m.thread.turnID = response.Turn.ID
	m.setState(stateRunning)
	return nil
}

func (m *model) applyLogin(value any) tea.Cmd {
	response, ok := value.(*rpc.LoginAccountResponse)
	if !ok {
		return m.unexpectedResult("login")
	}
	url := sanitizeText(cmp.Or(response.AuthURL, response.VerificationURL))
	if url == "" {
		m.noticef("login started; follow the provider instructions or run /login again to retry")
		return nil
	}
	notice := "login: open " + url + " in a browser; the TUI will resume when login completes"
	if response.UserCode != "" {
		notice += "; enter code " + response.UserCode
	}
	m.noticef("%s", notice)
	return nil
}

func (m *model) unexpectedResult(name string) tea.Cmd {
	m.errorf("%s", name+" response has an unexpected shape; reconnect and retry")
	return nil
}
