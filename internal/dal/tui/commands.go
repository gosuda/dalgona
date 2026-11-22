package tui

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime/debug"
	"slices"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	requestTimeout = 30 * time.Second
	maxFollowups   = 32
)

func requestContext(done <-chan struct{}) (context.Context, context.CancelFunc) {
	return boundedContext(done, requestTimeout)
}

func boundedContext(done <-chan struct{}, timeout time.Duration) (context.Context, context.CancelFunc) {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	if done == nil {
		return ctx, cancel
	}
	go func() {
		select {
		case <-done:
			cancel()
		case <-ctx.Done():
		}
	}()
	return ctx, cancel
}

func (m *model) call(op, method string, params, result any, text string) tea.Cmd {
	return m.callPrompt(op, method, params, result, followup{text: text})
}

func (m *model) callPrompt(op, method string, params, result any, prompt followup) tea.Cmd {
	turnID := m.thread.turnID
	return func() tea.Msg {
		if m.conn.client == nil {
			return resultMsg{op: op, text: prompt.text, verbatim: prompt.verbatim, err: errors.New("rpc client is not connected")}
		}
		ctx, cancel := requestContext(m.conn.done)
		defer cancel()
		err := m.conn.client.Call(ctx, method, params, result)
		return resultMsg{op: op, value: result, text: prompt.text, verbatim: prompt.verbatim, turnID: turnID, err: err}
	}
}

func (m *model) initialize() tea.Cmd {
	return func() tea.Msg {
		if m.conn.client == nil {
			return resultMsg{op: "initialize", err: errors.New("rpc client is not connected")}
		}
		ctx, cancel := requestContext(m.conn.done)
		defer cancel()
		params := rpc.InitializeParams{ClientInfo: rpc.ClientInfo{Name: "dalgona", Title: "dalgona TUI", Version: clientVersion()}}
		var result rpc.InitializeResult
		if err := m.conn.client.Call(ctx, "initialize", params, &result); err != nil {
			return resultMsg{op: "initialize", err: err}
		}
		return resultMsg{op: "initialize", value: &result, err: m.conn.client.Notify("initialized", nil)}
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
	var keywords []keyword
	return m.call("keywords", "dalgona/keywords/list", rpc.DalgonaKeywordsListParams{}, &keywords, "")
}

type settingValue struct {
	Key   string `json:"key"`
	Value any    `json:"value"`
}

type settingsReply struct {
	Settings []settingValue `json:"settings"`
}

func (m *model) readSettings() tea.Cmd {
	var reply settingsReply
	return m.call("settings", "dalgona/settings/read", struct{}{}, &reply, "")
}

func (m *model) applySettings(value any) tea.Cmd {
	reply, ok := value.(*settingsReply)
	if !ok {
		return m.unexpectedResult("settings")
	}
	for _, setting := range reply.Settings {
		m.applySetting(setting)
		m.applyNotifications(setting)
	}
	m.screen.setTheme()
	return m.requestKittyFont()
}

func (m *model) applySetting(setting settingValue) {
	switch setting.Key {
	case "theme":
		if text, ok := setting.Value.(string); ok {
			m.screen.themeSetting = text
		}
	case "icons":
		if text, ok := setting.Value.(string); ok {
			m.screen.iconsSetting = text
		}
	case "verbosity":
		if text, ok := setting.Value.(string); ok {
			m.screen.verbosity = text
		}
	case "statusHide":
		m.setStatusHide(setting.Value)
	default:
		m.rememberSetting(setting)
	}
}

func (m *model) applyNotifications(setting settingValue) {
	if setting.Key != "notifications" {
		return
	}
	if on, ok := setting.Value.(bool); ok {
		m.screen.notifications = on
	}
}

func (m *model) loadLevels() tea.Cmd {
	return m.call("levels", "model/list", rpc.ModelListParams{}, new(rpc.ModelListResponse), "")
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
		ApprovalPolicy: approvalPolicy(m.conn.opts.Approval),
		Sandbox:        sandboxPolicy(m.thread.sandbox),
		Cwd:            m.conn.opts.Cwd,
	}
	return m.callSeq(m.resetPendingResume(""), "new", "thread/start", params, new(rpc.ThreadStartResponse))
}

func approvalPolicy(mode string) string {
	switch mode {
	case "yolo":
		return "never"
	case "ask":
		return "untrusted"
	case "auto":
		return "on-request"
	}
	return ""
}

func approvalFromPolicy(policy string) string {
	switch policy {
	case "never":
		return "yolo"
	case "untrusted":
		return "ask"
	case "on-request", "on-failure":
		return "auto"
	}
	return ""
}

func sandboxPolicy(mode string) string {
	if mode == "off" {
		return "danger-full-access"
	}
	return "workspace-write"
}

func (m *model) submitPrompt() tea.Cmd {
	composer := m.input.composer
	text := strings.TrimSpace(composer.expand(composer.Value()))
	images := composer.images()
	if text == "" && len(images) == 0 {
		return m.startQueuedFollowup()
	}
	text, command, done := m.slashSubmit(composer, text)
	if done {
		return command
	}
	m.input.composer.SetValue("")
	if m.thread.id == "" {
		m.noticef("thread is not ready; wait for startup or use /new to retry")
		m.input.composer = composer
		return nil
	}
	m.input.history.rewind()
	if m.thread.state == stateRunning || m.thread.state == stateAwaitingApproval {
		return m.sendSteer(text, images)
	}
	return m.sendPrompt(text, images)
}

func (m *model) slashSubmit(composer editor, text string) (string, tea.Cmd, bool) {
	if !composer.readsSlash() {
		return text, nil, false
	}
	name, arg, kind := composer.slashParts()
	switch kind {
	case slashCommand:
		m.input.composer.SetValue("")
		return "", m.executeSlash(name, composer.expand(arg)), true
	case slashUnknown:
		m.noticef("unknown command /%s; start with // to send it as a prompt", name)
		return "", nil, true
	case slashPrompt:
		return composer.expand(arg), nil, false
	}
	return text, nil, false
}

func (m *model) sendPrompt(text string, images []chip) tea.Cmd {
	m.input.history.add(text, "")
	m.setState(stateRunning)
	m.thread.turnStartedAt = m.now()
	m.thread.turnSent = true
	params := rpc.TurnStartParams{ThreadID: m.thread.id, Input: promptInput(text, images), Model: m.thread.modelName}
	result := new(rpc.TurnStartResponse)
	return m.call("send", "turn/start", params, result, text)
}

func (m *model) sendSteer(text string, images []chip) tea.Cmd {
	m.input.history.add(text, m.thread.turnID)
	params := rpc.TurnSteerParams{ThreadID: m.thread.id, ExpectedTurnID: m.thread.turnID, Input: promptInput(text, images)}
	return m.call("steer", "turn/steer", params, new(rpc.TurnSteerResponse), text)
}

func textInput(text string) []rpc.InputItem {
	return []rpc.InputItem{{Type: "text", Text: text}}
}

func promptInput(text string, images []chip) []rpc.InputItem {
	var items []rpc.InputItem
	if text != "" {
		items = append(items, rpc.InputItem{Type: "text", Text: text})
	}
	for _, ch := range images {
		items = append(items, rpc.InputItem{Type: "image", URL: ch.image})
	}
	return items
}

func (m *model) queueFollowup() tea.Cmd {
	composer := m.input.composer
	text := strings.TrimSpace(composer.Expanded())
	if text == "" {
		return nil
	}
	if len(m.queue) >= maxFollowups {
		m.noticef("follow-up queue is full (%d); wait for the active turn or press Esc", maxFollowups)
		return nil
	}
	m.queue = append(m.queue, followup{text: text, verbatim: m.input.composer.sendsPastedSlash(text)})
	m.syncQueue()
	m.input.composer.SetValue("")
	m.noticef("queued follow-up %d of %d", len(m.queue), maxFollowups)
	if m.thread.state == stateIdle {
		return m.startQueuedFollowup()
	}
	return nil
}

func (m *model) dequeueFollowup() {
	last := len(m.queue) - 1
	next := m.queue[last]
	m.queue = m.queue[:last]
	m.syncQueue()
	m.input.composer.recall(next.text, next.verbatim)
}

func (m *model) startQueuedFollowup() tea.Cmd {
	if len(m.queue) == 0 || m.thread.state == stateRunning || m.thread.state == stateAwaitingApproval {
		return nil
	}
	next := m.queue[0]
	m.queue = m.queue[1:]
	m.input.history.add(next.text, "")
	m.syncQueue()
	m.setState(stateRunning)
	m.thread.turnStartedAt = m.now()
	m.thread.turnSent = true
	params := rpc.TurnStartParams{ThreadID: m.thread.id, Input: textInput(next.text), Model: m.thread.modelName}
	result := new(rpc.TurnStartResponse)
	return m.callPrompt("followup", "turn/start", params, result, next)
}

func (m *model) interrupt() tea.Cmd {
	if m.list != nil {
		m.closeList()
		return nil
	}
	if busyWord(m.thread.state) == "" {
		m.quietNotice(quietWord(m.thread.state))
		return nil
	}
	params := rpc.TurnInterruptParams{ThreadID: m.thread.id, TurnID: m.thread.turnID}
	return m.call("interrupt", "turn/interrupt", params, nil, "")
}

func (m *model) quietNotice(word string) {
	m.noticef("%s; nothing to interrupt", word)
}

func quietWord(state string) string {
	if state == stateIdle {
		return "idle"
	}
	return strings.ToLower(stateWord(state, 0))
}

func (m *model) applyResult(msg resultMsg) tea.Cmd {
	if cmd, handled := m.applyLoginResult(msg); handled {
		return cmd
	}
	if cmd, handled := m.applyPreludeResult(msg); handled {
		return cmd
	}
	if msg.err != nil {
		return m.applyResultError(msg)
	}
	return m.applySuccessResult(msg)
}

func (m *model) applyPreludeResult(msg resultMsg) (tea.Cmd, bool) {
	switch {
	case msg.op == "levels":
		m.applyLevels(msg)
		return nil, true
	case strings.HasPrefix(msg.op, "advisor"):
		return m.applyAdvisorResult(msg), true
	case (msg.op == "new" || msg.op == "fork") && msg.seq != 0 && msg.seq != m.pendingState().seq:
		return nil, true
	case msg.op == "initialize" && msg.err != nil:
		flat := strings.Join(strings.Fields(msg.err.Error()), " ")
		m.commit("welcome", []string{m.screen.fit(sanitizeText("Could not start: "+flat+". Press ctrl+c to exit."), terminalWidth(m.screen.width))})
		return nil, true
	}
	return nil, false
}

func (m *model) applySuccessResult(msg resultMsg) tea.Cmd {
	switch msg.op {
	case "keywords", "models", "threads", "tree", "revertTree", "forkTree", "rewindTree", "resumed", "new", "fork":
		return m.applyDataResult(msg)
	case "send", "followup":
		return m.applyTurnAccepted(msg.value)
	case "initialize":
		return m.applyInitialize(msg.value)
	case "settings":
		return m.applySettings(msg.value)
	case "settingsSave":
		return m.applySettingsSaved(msg)
	case "rewind":
		m.applyRewind(msg.text)
		return nil
	default:
		return m.applyConfirmation(msg)
	}
}

func (m *model) applyInitialize(value any) tea.Cmd {
	result, ok := value.(*rpc.InitializeResult)
	if !ok {
		return m.unexpectedResult("initialize")
	}
	product, version, _ := strings.Cut(result.UserAgent, "/")
	m.conn.product = sanitizeText(product)
	m.conn.version = sanitizeText(version)
	m.conn.platformFamily = sanitizeText(result.PlatformFamily)
	m.commit("welcome", m.welcomeLines())
	return nil
}

func (m *model) welcomeLines() []string {
	dim := lipgloss.NewStyle().Faint(true)
	details := slices.DeleteFunc([]string{m.conn.version, cmp.Or(m.thread.modelName, "default"), displayCwd(m.workDir())}, func(s string) bool { return s == "" })
	name := lipgloss.NewStyle().Bold(true).Render(m.conn.product)
	return []string{name + dim.Render(" "+sanitizeText(strings.Join(details, " · "))), dim.Render("Type a prompt · / commands · ? keys")}
}

func (m *model) workDir() string {
	if m.thread.id != "" {
		return m.thread.cwd
	}
	return m.conn.opts.Cwd
}

func displayCwd(cwd string) string {
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		return cwd
	}
	if rest, ok := strings.CutPrefix(cwd, home); ok && (rest == "" || rest[0] == filepath.Separator) {
		return "~" + rest
	}
	return cwd
}

func (m *model) applyLevels(msg resultMsg) {
	response, ok := msg.value.(*rpc.ModelListResponse)
	if ok && msg.err == nil {
		m.rememberLevels(response.Data)
	}
}

type resumedThread struct {
	rpc.ThreadReadResponse
	effort       string
	turnEffort   *string
	turnRevision *uint64
	approval     string
	model        string
	cwd          string
	resumeSeq    int
}

func (m *model) applyDataResult(msg resultMsg) tea.Cmd {
	switch msg.op {
	case "keywords":
		return m.applyKeywords(msg.value)
	case "models", "threads", "tree", "revertTree", "forkTree", "rewindTree":
		return m.applyListReply(msg)
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
		m.advisorFailed(msg.err)
		return nil
	}
	states, _ := msg.value.(*[]rpc.AdvisorState)
	switch msg.op {
	case "advisorPanel":
		return m.applyAdvisorPanel(msg, states)
	case "advisorStatus":
		if states == nil {
			return m.unexpectedResult("advisor status")
		}
		m.noticef("%s", advisorStatusText(*states))
	case "advisorSet":
		m.advisorToggled(msg.text == "true")
	case "advisorReset":
		m.finishAdvisorList()
		m.noticef("advisor reset; it reviews the current branch at the next turn")
	}
	return nil
}

func (m *model) applyAdvisorPanel(msg resultMsg, states *[]rpc.AdvisorState) tea.Cmd {
	if m.staleListReply(msg) {
		return nil
	}
	if states == nil {
		return m.unexpectedResult("advisor status")
	}
	if len(*states) == 0 {
		m.noticef("%s", advisorStatusText(*states))
		return nil
	}
	m.openAdvisorList(*states)
	return nil
}

func (m *model) advisorFailed(err error) {
	if m.list != nil && m.list.kind == "advisor" {
		m.list.err = advisorFailure(err)
		return
	}
	m.errorf("%s", advisorFailure(err))
}

func (m *model) advisorToggled(enabled bool) {
	m.finishAdvisorList()
	if enabled {
		m.noticef("advisor on; it reviews after each turn")
		return
	}
	m.noticef("advisor off; run /advisor to turn it back on")
}

func advisorFailure(err error) string {
	if rpcErr, ok := errors.AsType[*rpc.RPCError](err); ok && rpcErr.Code == -32001 {
		return sanitizeText("advisor unavailable: " + rpcErr.Message + "; set [advisor] enabled = true in config.toml, then restart dalgona")
	}
	return sanitizeText(fmt.Sprintf("advisor request failed: %v; retry the /advisor command", err))
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
		return m.applyInterrupt(msg)
	case "review":
		m.finishReview()
	case "compact":
		m.noticef("compaction started; wait for the context compaction item")
	case "revert":
		m.noticef("files changed by later turns are not reverted")
	case "leaf":
		m.noticef("active branch changed; files changed by later turns are not reverted")
	case "modelSet", "modelEffort":
		return m.applyModelResult(msg)
	case "steer":
		m.noticef("steering message accepted")
	}
	return nil
}

func (m *model) applyInterrupt(msg resultMsg) tea.Cmd {
	if msg.turnID != m.thread.turnID {
		return nil
	}
	m.setState(stateInterrupted)
	m.noticef("interrupt requested; wait for turn completion before sending")
	return nil
}

func (m *model) applyRewind(label string) {
	if m.input.composer.Value() != "" {
		m.noticef("Rewound; the editor holds a draft, so the prompt was not restored")
		return
	}
	m.input.composer.SetValue(label)
	if utf8.RuneCountInString(label) == 80 {
		m.noticef("Draft holds the first 80 characters of that prompt")
	}
}

func (m *model) panelFailed(msg resultMsg) bool {
	return m.saveFailed(msg) || m.settingsFailed(msg) || m.reviewFailed(msg)
}

func (m *model) applyResultError(msg resultMsg) tea.Cmd {
	if msg.op == "resumed" && m.concludePendingResume(msg.seq) {
		return nil
	}
	if m.panelFailed(msg) {
		return nil
	}
	m.restorePrompt(msg)
	if msg.op == "keywords" {
		m.errorf("keyword highlighting unavailable: %v; prompting is ready, restart to retry", msg.err)
		return nil
	}
	m.errorf("%s failed: %v; retry, %s, or /new", resultLabel(msg.op), msg.err, m.resumeHint())
	return nil
}

func (m *model) restorePrompt(msg resultMsg) {
	switch msg.op {
	case "send", "steer":
		m.restoreComposer(msg.text)
	case "followup":
		m.queue = append([]followup{{text: msg.text, verbatim: msg.verbatim}}, m.queue...)
		m.syncQueue()
	}
	if msg.op == "send" || msg.op == "followup" {
		m.setState(stateFailed)
		m.thread.turnSent = false
	}
}

func resultLabel(op string) string {
	switch op {
	case "tree", "revertTree", "rewindTree":
		return "turn list"
	}
	return op
}

func (m *model) restoreComposer(text string) {
	verbatim := strings.HasPrefix(strings.TrimSpace(text), "/")
	current := m.input.composer.Expanded()
	if current != "" {
		text += "\n" + current
	}
	m.input.composer.recall(text, verbatim)
}

func (m *model) applyKeywords(value any) tea.Cmd {
	keywords, ok := value.(*[]keyword)
	if !ok {
		m.errorf("keyword list has an unexpected shape; restart the TUI to retry")
		return nil
	}
	m.input.keywords = *keywords
	return nil
}

type turnItem struct {
	turn string
	item rpc.Item
}

func (m *model) applyResumed(value any) tea.Cmd {
	response, ok := value.(*resumedThread)
	if !ok {
		return m.unexpectedResult("resumed thread")
	}
	held, proceed := m.claimPendingResume(response)
	if !proceed {
		return nil
	}
	if response.turnRevision != nil && m.thread.id == response.Thread.ID && *response.turnRevision != m.thread.turnRevision {
		return nil
	}
	m.adoptResumed(response)
	return m.replayResumed(response, held)
}

func (m *model) claimPendingResume(response *resumedThread) ([]notificationMsg, bool) {
	p := m.pending
	if p == nil {
		return nil, true
	}
	if response.resumeSeq != 0 && response.resumeSeq != p.seq {
		return nil, false
	}
	var held []notificationMsg
	if p.target == response.Thread.ID {
		if p.failed {
			m.clearPendingResume()
			return nil, false
		}
		held = p.held
	}
	m.clearPendingResume()
	return held, true
}

func (m *model) adoptResumed(response *resumedThread) {
	m.setThread(response.Thread.ID)
	m.thread.cwd = response.cwd
	m.thread.approvalMode = approvalFromPolicy(response.approval)
	m.thread.effort = response.effort
	m.thread.modelName = response.model
	m.thread.title = threadTitle(response.Thread)
	m.noticeMoved(m.thread.title)
	m.setState(threadState(response.Thread.Status))
	m.live.items = nil
	m.live.byID = nil
}

func (m *model) replayResumed(response *resumedThread, held []notificationMsg) tea.Cmd {
	items := snapshotItems(response.Thread.Turns)
	m.out.loads++
	m.commit(threadKey(m.thread.id, m.out.loads), []string{m.threadSeparator()})
	if len(items) > maxSettledItems {
		m.commit(fmt.Sprintf("earlier:%s:%d", m.thread.id, m.out.loads), []string{fmt.Sprintf("%d earlier items are not shown", len(items)-maxSettledItems)})
		items = items[len(items)-maxSettledItems:]
	}
	m.settleResumedItems(items)
	m.restoreActiveTurn(response)
	command := m.replayHeld(held)
	m.emitStatus()
	return command
}

func snapshotItems(turns []rpc.Turn) []turnItem {
	var items []turnItem
	for _, turn := range turns {
		for _, item := range turn.Items {
			items = append(items, turnItem{turn: turn.ID, item: item})
		}
	}
	return items
}

func (m *model) settleResumedItems(items []turnItem) {
	var turn string
	for _, entry := range items {
		if entry.item.Type == "userMessage" {
			m.input.history.add(entry.item.Text, entry.turn)
		}
		if entry.turn != turn {
			m.flushGroup()
			turn = entry.turn
			m.thread.turnID = turn
		}
		m.settleItem(itemKey(m.thread.id, entry.turn, entry.item.ID), entry.item, nil)
	}
	m.flushGroup()
	m.thread.turnID = ""
	m.thread.turnEffort = nil
}

func (m *model) restoreActiveTurn(response *resumedThread) {
	for _, turn := range response.Thread.Turns {
		if turn.Status == "inProgress" {
			m.thread.turnID = turn.ID
			m.thread.turnEffort = response.turnEffort
		}
	}
}

func (m *model) replayHeld(held []notificationMsg) tea.Cmd {
	var cmds []tea.Cmd
	for _, note := range held {
		command, err := m.applyDecodedNotification(note)
		if err != nil {
			m.protocolError(note.method, err)
			continue
		}
		cmds = append(cmds, command)
	}
	return tea.Sequence(cmds...)
}

func (m *model) applyNewThread(value any) tea.Cmd {
	response, ok := value.(*rpc.ThreadStartResponse)
	if !ok {
		return m.unexpectedResult("new thread")
	}
	m.resetPendingResume("")
	m.setThread(response.Thread.ID)
	m.thread.cwd = response.Cwd
	m.thread.approvalMode = approvalFromPolicy(response.ApprovalPolicy)
	m.thread.turnEffort = nil
	m.thread.effort = response.ReasoningEffort
	m.thread.modelName = response.Model
	m.thread.title = threadTitle(response.Thread)
	m.noticeMoved(m.thread.title)
	m.thread.sandbox = serverSandbox(response.Sandbox.Type, m.thread.sandbox)
	m.live.items = nil
	m.live.byID = nil
	m.setState(stateIdle)
	m.out.loads++
	m.commit(threadKey(m.thread.id, m.out.loads), []string{"started " + m.thread.title})
	return nil
}

func threadKey(threadID string, load int) string {
	return fmt.Sprintf("thread:%s:%d", threadID, load)
}

func (m *model) threadSeparator() string {
	return m.screen.fit("── "+m.thread.title+" ──", terminalWidth(m.screen.width))
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
	m.noticeMoved(threadTitle(response.Thread))
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
	m.input.history.startTurn(response.Turn.ID)
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
		return nil
	}
	notice := "Open " + url + " to sign in"
	if response.UserCode != "" {
		notice += "; enter code " + response.UserCode
	}
	m.noticef("%s; waiting for the provider to confirm", notice)
	return nil
}

func (m *model) unexpectedResult(name string) tea.Cmd {
	m.errorf("%s", name+" response has an unexpected shape; reconnect and retry")
	return nil
}

type shellParams struct {
	Command        []string `json:"command"`
	Cwd            string   `json:"cwd"`
	TimeoutMS      int64    `json:"timeoutMs"`
	OutputBytesCap int64    `json:"outputBytesCap"`
}

const (
	shellTimeoutMS = 120000
	shellOutputCap = 65536
	shellMargin    = 10 * time.Second
)

func (m *model) runShell() tea.Cmd {
	command := strings.TrimSpace(m.input.composer.Expanded())
	if command == "" {
		return nil
	}
	m.input.composer.SetValue("")
	m.input.shells++
	return m.shellCommand(m.input.shells, command)
}

func (m *model) shellCommand(n int, command string) tea.Cmd {
	argv := []string{"sh", "-c", command}
	if m.conn.platformFamily == "windows" {
		argv = []string{"cmd", "/C", command}
	}
	params := shellParams{Command: argv, Cwd: m.workDir(), TimeoutMS: shellTimeoutMS, OutputBytesCap: shellOutputCap}
	client, done, clock := m.conn.client, m.conn.done, m.conn.clock
	return func() tea.Msg {
		if client == nil {
			return shellResultMsg{n: n, command: command, err: errors.New("rpc client is not connected")}
		}
		ctx, cancel := boundedContext(done, shellTimeoutMS*time.Millisecond+shellMargin)
		defer cancel()
		started := clock()
		var result rpc.CommandExecResponse
		err := client.Call(ctx, "command/exec", params, &result)
		return shellResultMsg{n: n, command: command, result: result, err: err, elapsed: clock().Sub(started)}
	}
}

func (m *model) applyShell(msg shellResultMsg) {
	key := "shell:" + strconv.Itoa(msg.n)
	ms := msg.elapsed.Milliseconds()
	command := sanitizeText(msg.command)
	if msg.err != nil {
		m.commit(key, m.renderRow(row{label: "!", subject: command, state: "Failed", duration: durationText(&ms), body: textLines(sanitizeText(msg.err.Error()))}))
		return
	}
	output := shellOutput(msg.result)
	exit := msg.result.ExitCode
	status := "completed"
	if exit != 0 {
		status = "failed"
	}
	item := rpc.Item{Type: "commandExecution", Command: command, Status: status, Output: output, ExitCode: &exit, DurationMs: &ms}
	r := commandRow(item, m.screen.level())
	r.mark, r.label = "", "!"
	m.commit(key, m.renderRow(r))
	if !m.draftHoldsOnlyShellOutput() {
		return
	}
	label := fmt.Sprintf("[Shell #%d exit %d, %s]", msg.n, exit, pluralText(len(textLines(output)), "line"))
	m.input.composer.addChip(chip{label: label, text: shellExpansion(command, exit, output), shell: true})
}

func (m *model) draftHoldsOnlyShellOutput() bool {
	composer := &m.input.composer
	return !slices.ContainsFunc(composer.text, func(r rune) bool {
		ch, ok := composer.chipOf(r)
		return !ok || !ch.shell
	})
}

func shellOutput(result rpc.CommandExecResponse) string {
	var parts []string
	for _, text := range []string{result.Stdout, result.Stderr} {
		if text != "" {
			parts = append(parts, strings.TrimSuffix(text, "\n"))
		}
	}
	return sanitizeText(strings.Join(parts, "\n"))
}

func shellExpansion(command string, exit int, output string) string {
	fence := strings.Repeat("`", max(3, longestBacktickRun(output)+1))
	span := "`"
	if strings.Contains(command, "`") {
		span, command = "``", " "+command+" "
	}
	return fmt.Sprintf("Output of %s%s%s (exit %d):\n%s\n%s\n%s", span, command, span, exit, fence, output, fence)
}

func longestBacktickRun(text string) int {
	longest, run := 0, 0
	for _, r := range text {
		if r != '`' {
			run = 0
			continue
		}
		run++
		longest = max(longest, run)
	}
	return longest
}
