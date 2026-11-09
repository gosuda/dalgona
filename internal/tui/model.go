package tui

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"fmt"
	"slices"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/glamour/v2"
	"charm.land/lipgloss/v2"

	"github.com/gosuda/dalgona/internal/rpc"
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
	client         caller
	opts           Options
	done           <-chan struct{}
	composer       composer
	width          int
	focused        bool
	phase          float64
	shimmerActive  bool
	keywords       []Keyword
	live           []liveItem
	deltas         *deltaDecoder
	pendingCommand tea.Cmd
	approvals      []approval
	selector       *selector
	markdown       map[int]*glamour.TermRenderer
	state          string
	seq            uint64
	threadID       string
	turnID         string
	title          string
	modelName      string
	approvalMode   string
	sandbox        string
	notice         string
	followups      []string
	ctrlCArmed     bool
}

type approval struct {
	method   string
	threadID string
	itemID   string
	command  string
	reason   string
	answer   chan requestAnswer
}

type selector struct {
	kind    string
	title   string
	choices []choice
	cursor  int
}

type choice struct {
	id     string
	label  string
	detail string
	active bool
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
		client:       client,
		opts:         opts,
		done:         done,
		composer:     newComposer(),
		width:        80,
		focused:      true,
		markdown:     make(map[int]*glamour.TermRenderer),
		deltas:       new(deltaDecoder),
		state:        stateIdle,
		title:        "new thread",
		modelName:    opts.Model,
		approvalMode: approvalMode,
		sandbox:      sandbox,
	}
}

func (m model) Init() tea.Cmd {
	start := tea.Sequence(m.initialize(), m.loadKeywords(), m.startThread())
	return tea.Batch(m.composer.Focus(), start)
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	var command tea.Cmd
	switch msg := msg.(type) {
	case tea.KeyPressMsg:
		command = m.updateKey(msg)
	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.composer.SetWidth(msg.Width)
	case tea.FocusMsg:
		m.focused = true
		m.composer.focused = true
	case tea.BlurMsg:
		m.focused = false
		m.composer.focused = false
	case notificationMsg, serverRequestMsg, resultMsg, shimmerMsg, disconnectedMsg:
		command = m.updateAsync(msg)
	default:
		command = m.updateComposer(msg)
	}
	if _, ok := msg.(shimmerMsg); !ok {
		command = tea.Batch(command, m.scheduleShimmer())
	}
	return m, command
}

func (m *model) updateAsync(msg tea.Msg) tea.Cmd {
	switch msg := msg.(type) {
	case notificationMsg:
		return m.applyNotification(msg)
	case serverRequestMsg:
		m.applyRequest(msg)
	case resultMsg:
		return m.applyResult(msg)
	case shimmerMsg:
		return m.advanceShimmer()
	case disconnectedMsg:
		m.setState(stateDisconnected)
		m.notice = fmt.Sprintf("connection lost: %v; restart the TUI, then %s", msg.err, m.resumeHint())
	}
	return nil
}

func (m *model) updateKey(msg tea.KeyPressMsg) tea.Cmd {
	if msg.String() == "ctrl+c" {
		return m.confirmExit()
	}
	m.ctrlCArmed = false
	if len(m.approvals) > 0 {
		return m.answerApprovalKey(msg.String())
	}
	if m.selector != nil {
		return m.selectKey(msg.String())
	}
	switch msg.String() {
	case "enter":
		return m.submitPrompt()
	case "alt+enter":
		return m.queueFollowup()
	case "esc":
		return m.interrupt()
	default:
		return m.updateComposer(msg)
	}
}

func (m *model) confirmExit() tea.Cmd {
	if m.ctrlCArmed {
		return tea.Quit
	}
	m.ctrlCArmed = true
	m.notice = "press Ctrl+C again to exit; Esc interrupts the active turn"
	return nil
}

func (m *model) updateComposer(msg tea.Msg) tea.Cmd {
	updated, command := m.composer.Update(msg)
	m.composer = updated
	return command
}

func (m *model) applyRequest(msg serverRequestMsg) {
	request, err := decodeApproval(msg.method, msg.params)
	if err != nil {
		msg.answer <- requestAnswer{err: err}
		m.notice = fmt.Sprintf("approval request failed: %v; reconnect and retry the turn", err)
		return
	}
	request.answer = msg.answer
	m.approvals = append(m.approvals, request)
	if m.acceptThread(request.threadID) {
		m.setState(stateAwaitingApproval)
	}
}

func (m *model) answerApprovalKey(key string) tea.Cmd {
	var decision string
	switch key {
	case "y":
		decision = "accept"
	case "a":
		decision = "acceptForSession"
	case "n":
		decision = "decline"
	case "esc":
		decision = "cancel"
	default:
		return nil
	}
	m.approvals[0].answer <- requestAnswer{value: rpc.CommandExecutionRequestApprovalResponse{Decision: decision}}
	m.approvals = m.approvals[1:]
	if len(m.approvals) == 0 {
		m.setState(stateRunning)
	}
	if key == "esc" {
		return m.interrupt()
	}
	return nil
}

func (m *model) applyNotification(msg notificationMsg) tea.Cmd {
	m.pendingCommand = nil
	if err := m.applyDecodedNotification(msg); err != nil {
		m.notice = fmt.Sprintf("protocol error in %s: %v; reconnect with %s or use /new", msg.method, err, m.resumeHint())
		return nil
	}
	m.emitStatus()
	return m.pendingCommand
}

func (m *model) applyDecodedNotification(msg notificationMsg) error {
	switch msg.method {
	case "thread/started", "thread/status/changed", "thread/name/updated", "thread/reverted":
		return m.applyThreadNotification(msg)
	case "turn/started", "turn/completed", "turn/plan/updated":
		return m.applyTurnNotification(msg)
	case "item/started", "item/completed", "item/agentMessage/delta", "item/reasoning/textDelta", "item/commandExecution/outputDelta":
		return m.applyItemNotification(msg)
	case "error":
		return m.applyError(msg.params)
	case "dalgona/thread/childSpawned":
		return m.applyChildSpawned(msg.params)
	case "dalgona/advisor/note":
		return m.applyAdvisorNote(msg.params)
	default:
		return nil
	}
}

func (m *model) applyThreadNotification(msg notificationMsg) error {
	switch msg.method {
	case "thread/started":
		return m.applyThreadStarted(msg.params)
	case "thread/status/changed":
		return m.applyThreadStatus(msg.params)
	case "thread/name/updated":
		return m.applyThreadName(msg.params)
	default:
		m.notice = "files changed by later turns are not reverted"
		return nil
	}
}

func (m *model) applyTurnNotification(msg notificationMsg) error {
	switch msg.method {
	case "turn/started":
		return m.applyTurnStarted(msg.params)
	case "turn/completed":
		return m.applyTurnCompleted(msg.params)
	default:
		return m.applyPlan(msg.params)
	}
}

func (m *model) applyItemNotification(msg notificationMsg) error {
	switch msg.method {
	case "item/started":
		return m.applyItemStarted(msg.params)
	case "item/completed":
		return m.applyItemCompleted(msg.params)
	default:
		return m.applyDelta(msg.method, msg.params)
	}
}

func (m *model) applyThreadStarted(params jsontext.Value) error {
	var event rpc.ThreadStartedNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if !m.acceptThread(event.Thread.ID) {
		return nil
	}
	m.title = threadTitle(event.Thread)
	m.setState(threadState(event.Thread.Status))
	return nil
}

func (m *model) applyThreadStatus(params jsontext.Value) error {
	var event rpc.ThreadStatusChangedNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) || event.Seq > 0 && event.Seq < m.seq {
		return nil
	}
	m.seq = max(m.seq, event.Seq)
	m.setState(threadState(event.Status))
	return nil
}

func (m *model) applyThreadName(params jsontext.Value) error {
	var event rpc.ThreadNameUpdatedNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if m.acceptThread(event.ThreadID) {
		m.title = cmp.Or(event.ThreadName, event.ThreadID)
	}
	return nil
}

func (m *model) applyChildSpawned(params jsontext.Value) error {
	var event rpc.ChildSpawnedNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if m.acceptThread(event.ThreadID) && m.opts.OnChildSpawned != nil {
		m.opts.OnChildSpawned(event.ChildID, event.Cwd)
	}
	return nil
}

func (m *model) applyAdvisorNote(params jsontext.Value) error {
	var event rpc.AdvisorNoteNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if m.acceptThread(event.ThreadID) {
		m.pendingCommand = tea.Println(m.renderAdvisorCard(event.Advisor, event.Severity, event.Note))
	}
	return nil
}

func (m *model) applyTurnStarted(params jsontext.Value) error {
	var event rpc.TurnStartedNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) {
		return nil
	}
	m.turnID = event.Turn.ID
	m.notice = ""
	m.setState(stateRunning)
	return nil
}

func (m *model) applyTurnCompleted(params jsontext.Value) error {
	var event rpc.TurnCompletedNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) {
		return nil
	}
	m.turnID = ""
	m.setState(turnState(event.Turn.Status))
	if m.state != stateIdle {
		if len(m.followups) > 0 {
			m.notice = fmt.Sprintf("turn %s; %d follow-up held, press Enter on an empty composer to send it", m.state, len(m.followups))
		}
		return nil
	}
	m.pendingCommand = m.startQueuedFollowup()
	return nil
}

func (m *model) applyItemStarted(params jsontext.Value) error {
	var event rpc.ItemStartedNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) {
		return nil
	}
	live := m.liveItem(event.Item.ID)
	live.Item = mergeItem(event.Item, live.Item)
	return nil
}

func (m *model) applyItemCompleted(params jsontext.Value) error {
	var event rpc.ItemCompletedNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) {
		return nil
	}
	item := m.liveItem(event.Item.ID).settle(event.Item)
	m.live = slices.DeleteFunc(m.live, func(live liveItem) bool { return live.ID == item.ID })
	if !isAdvisorItem(item) {
		m.pendingCommand = tea.Println(m.renderSettledItem(item))
	}
	return nil
}

func (m *model) liveItem(id string) *liveItem {
	index := slices.IndexFunc(m.live, func(item liveItem) bool { return item.ID == id })
	if index < 0 {
		m.live = append(m.live, liveItem{Item: rpc.Item{ID: id}})
		index = len(m.live) - 1
	}
	return &m.live[index]
}

func mergeItem(item, live rpc.Item) rpc.Item {
	item.Text = cmp.Or(item.Text, live.Text)
	item.Output = cmp.Or(item.Output, live.Output)
	item.Command = cmp.Or(item.Command, live.Command)
	item.Type = cmp.Or(item.Type, live.Type)
	return item
}

func (m *model) applyDelta(method string, params jsontext.Value) error {
	delta := m.deltas
	if err := delta.decode(params); err != nil {
		return err
	}
	if !m.acceptThread(delta.threadID) {
		return nil
	}
	item := m.liveItem(delta.itemID)
	item.Type = deltaType(method)
	if item.Type == "commandExecution" {
		item.output = append(item.output, delta.text...)
	} else {
		item.text = append(item.text, delta.text...)
	}
	return nil
}

func deltaType(method string) string {
	switch method {
	case "item/commandExecution/outputDelta":
		return "commandExecution"
	case "item/reasoning/textDelta":
		return "reasoning"
	default:
		return "agentMessage"
	}
}

func (m *model) applyPlan(params jsontext.Value) error {
	var event rpc.TurnPlanUpdatedNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) {
		return nil
	}
	lines := make([]string, 0, len(event.Plan)+1)
	lines = append(lines, "plan")
	for _, step := range event.Plan {
		lines = append(lines, fmt.Sprintf("[%s] %s", planStatus(step.Status), step.Step))
	}
	m.notice = strings.Join(lines, "\n")
	return nil
}

func planStatus(status string) string {
	switch status {
	case "inProgress":
		return "in progress"
	case "completed":
		return "done"
	default:
		return status
	}
}

func (m *model) applyError(params jsontext.Value) error {
	var event rpc.ErrorNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) {
		return nil
	}
	if event.WillRetry {
		m.notice = fmt.Sprintf("error: %s; retrying the turn, no action needed", event.Error.Message)
		return nil
	}
	m.setState(stateFailed)
	m.notice = fmt.Sprintf("error: %s; retry the turn, %s, or /new", event.Error.Message, m.resumeHint())
	return nil
}

func (m *model) acceptThread(id string) bool {
	if m.threadID == "" {
		m.setThread(id)
	}
	return id == m.threadID
}

func (m *model) setThread(id string) {
	if id != m.threadID {
		m.threadID = id
		m.turnID = ""
		m.seq = 0
		m.followups = nil
	}
}

func (m *model) resumeHint() string {
	if m.threadID == "" {
		return "/resume"
	}
	return "/resume " + m.threadID
}

func threadTitle(thread rpc.Thread) string {
	if thread.Name == "" {
		return thread.ID
	}
	return thread.Name
}

func threadState(status rpc.ThreadStatus) string {
	switch status.Type {
	case "active":
		if slices.Contains(status.ActiveFlags, "waitingOnApproval") {
			return stateAwaitingApproval
		}
		return stateRunning
	case "systemError":
		return stateFailed
	default:
		return stateIdle
	}
}

func turnState(status string) string {
	switch status {
	case "inProgress":
		return stateRunning
	case "interrupted":
		return stateInterrupted
	case "failed":
		return stateFailed
	default:
		return stateIdle
	}
}

func (m *model) setState(state string) {
	if m.state != stateDisconnected {
		m.state = state
	}
	m.emitStatus()
}

func (m *model) emitStatus() {
	if m.opts.OnStatus != nil {
		m.opts.OnStatus(Status{State: m.state, Seq: m.seq, ThreadID: m.threadID})
	}
}

func (m *model) scheduleShimmer() tea.Cmd {
	if m.shimmerActive || !m.shimmerVisible() {
		return nil
	}
	m.shimmerActive = true
	return tea.Tick(100*time.Millisecond, func(time.Time) tea.Msg { return shimmerMsg{} })
}

func (m *model) advanceShimmer() tea.Cmd {
	if !m.shimmerVisible() {
		m.shimmerActive = false
		return nil
	}
	m.phase = wrappedPhase(m.phase + 1.0/14.0)
	return tea.Tick(100*time.Millisecond, func(time.Time) tea.Msg { return shimmerMsg{} })
}

func (m *model) shimmerVisible() bool {
	return m.focused && len(matchKeywords(m.composer.textView(), m.keywords)) > 0
}

func (m model) View() tea.View {
	view := tea.NewView(m.renderView())
	view.AltScreen = false
	view.ReportFocus = true
	return view
}

func (m *model) renderView() string {
	sections := make([]string, 0, 6)
	if live := m.renderLiveItems(); live != "" {
		sections = append(sections, live)
	}
	if len(m.approvals) > 0 {
		sections = append(sections, m.renderApproval())
	}
	if m.selector != nil {
		sections = append(sections, m.renderSelector())
	}
	if m.notice != "" {
		sections = append(sections, wrapTerminal(m.notice, m.width))
	}
	sections = append(sections, "state: "+m.state)
	composer := glowKeywords(m.composer.textView(), m.keywords, m.composerPhase()) + m.composer.placeholderView()
	sections = append(sections, wrapTerminal(composer, m.width))
	sections = append(sections, m.statusRow())
	return strings.Join(sections, "\n")
}

func (m *model) composerPhase() float64 {
	if !m.focused {
		return 0
	}
	return m.phase
}

func (m *model) statusRow() string {
	sandbox := m.sandbox
	if sandbox == "off" || sandbox == "" {
		sandbox = "no sandbox"
	}
	status := fmt.Sprintf("%s · %s · %s · %s", cmp.Or(m.modelName, "default"), m.approvalMode, sandbox, m.title)
	if len(m.followups) > 0 {
		status += fmt.Sprintf(" · %d follow-up queued", len(m.followups))
	}
	return wrapTerminal(status, m.width)
}

func (m *model) renderLiveItems() string {
	lines := make([]string, 0, len(m.live))
	for _, item := range m.live {
		if isAdvisorItem(item.Item) {
			continue
		}
		lines = append(lines, wrapTerminal(item.view(), m.width))
	}
	return strings.Join(lines, "\n")
}

func (m *model) renderApproval() string {
	current := m.approvals[0]
	lines := []string{"approval required"}
	if len(m.approvals) > 1 {
		lines[0] += fmt.Sprintf(" (1 of %d)", len(m.approvals))
	}
	if current.threadID != m.threadID {
		lines = append(lines, "thread "+current.threadID)
	}
	if current.command != "" {
		lines = append(lines, "$ "+sanitizeText(current.command))
	}
	if current.reason != "" {
		lines = append(lines, "reason: "+sanitizeText(current.reason))
	}
	lines = append(lines, approvalChoices(m.width)...)
	return wrapTerminal(strings.Join(lines, "\n"), m.width)
}

func approvalChoices(width int) []string {
	choices := []string{"[y] accept", "[a] accept for session", "[n] decline", "[esc] cancel and interrupt"}
	if line := strings.Join(choices, "  "); lipgloss.Width(line) <= width {
		return []string{line}
	}
	return choices
}

func (m *model) renderSelector() string {
	lines := []string{m.selector.title, "↑/↓ choose · Enter select · Esc cancel"}
	for i, item := range m.selector.choices {
		marker := "  "
		if i == m.selector.cursor {
			marker = "› "
		}
		active := ""
		if item.active {
			active = " (active)"
		}
		line := marker + item.label + active
		if item.detail != "" {
			line += " — " + item.detail
		}
		lines = append(lines, line)
	}
	return wrapTerminal(strings.Join(lines, "\n"), m.width)
}
