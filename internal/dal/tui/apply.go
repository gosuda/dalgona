package tui

import (
	"cmp"
	"encoding/json/jsontext"
	"fmt"
	"slices"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func (m *model) applyNotification(msg notificationMsg) tea.Cmd {
	if m.holdPendingResume(msg) {
		return nil
	}
	command, err := m.applyDecodedNotification(msg)
	if err != nil {
		m.protocolError(msg.method, err)
		return nil
	}
	m.emitStatus()
	return command
}

func (m *model) holdPendingResume(msg notificationMsg) bool {
	p := m.pending
	if p == nil || p.target == "" || p.failed {
		return false
	}
	if notifyThreadID(msg.params) != p.target {
		return false
	}
	if len(p.held) >= pendingResumeLimit {
		p.failed = true
		p.held = nil
		m.errorf("resume to %s aborted: more than %d live notifications arrived before the snapshot; run /resume again", p.target, pendingResumeLimit)
		return true
	}
	p.held = append(p.held, msg)
	return true
}

type threadIDProbe struct {
	ThreadID string `json:"threadId"`
	Thread   struct {
		ID string `json:"id"`
	} `json:"thread"`
}

func notifyThreadID(params jsontext.Value) string {
	probe, err := decodeParams[threadIDProbe](params)
	if err != nil {
		return ""
	}
	return cmp.Or(probe.ThreadID, probe.Thread.ID)
}

func (m *model) pendingState() *pendingResume {
	if m.pending == nil {
		m.pending = &pendingResume{}
	}
	return m.pending
}

func (m *model) resetPendingResume(target string) int {
	p := m.pendingState()
	p.seq++
	p.target, p.held, p.failed = target, nil, false
	return p.seq
}

func (m *model) clearPendingResume() {
	if m.pending == nil {
		return
	}
	m.pending.target, m.pending.held, m.pending.failed = "", nil, false
}

func (m *model) concludePendingResume(seq int) bool {
	p := m.pending
	if p == nil || seq == 0 {
		return false
	}
	if seq != p.seq {
		return true
	}
	m.clearPendingResume()
	return false
}

func (m *model) applyDecodedNotification(msg notificationMsg) (tea.Cmd, error) {
	switch msg.method {
	case "thread/started", "thread/status/changed", "thread/name/updated", "thread/reverted", "thread/tokenUsage/updated", "serverRequest/resolved":
		return nil, m.applyThreadNotification(msg)
	case "turn/started", "turn/completed", "turn/plan/updated", "dalgona/retry/wait":
		return m.applyTurnNotification(msg)
	case "item/started", "item/completed", "item/agentMessage/delta", "item/reasoning/textDelta", "item/commandExecution/outputDelta", "item/fileChange/patchUpdated":
		return nil, m.applyItemNotification(msg)
	default:
		return m.applyEventNotification(msg)
	}
}

func (m *model) applyEventNotification(msg notificationMsg) (tea.Cmd, error) {
	switch msg.method {
	case "error":
		return nil, m.applyError(msg.params)
	case "dalgona/thread/childSpawned":
		return nil, m.applyChildSpawned(msg.params)
	case "dalgona/advisor/note", "dalgona/tool/judged", "dalgona/turn/judged":
		return nil, m.applyDalgonaNotification(msg)
	case "dalgona/turn/effort":
		return nil, m.applyTurnEffort(msg.params)
	case "account/login/completed":
		return nil, m.applyLoginCompleted(msg.params)
	default:
		return nil, nil
	}
}

func (m *model) applyDalgonaNotification(msg notificationMsg) error {
	switch msg.method {
	case "dalgona/advisor/note":
		return m.applyAdvisorNote(msg.params)
	case "dalgona/tool/judged":
		return m.applyToolJudged(msg.params)
	case "dalgona/turn/judged":
		return m.applyTurnJudged(msg.params)
	}
	return nil
}

func (m *model) applyThreadNotification(msg notificationMsg) error {
	switch msg.method {
	case "serverRequest/resolved":
		return m.applyRequestResolved(msg.params)
	case "thread/started":
		return m.applyThreadStarted(msg.params)
	case "thread/status/changed":
		return m.applyThreadStatus(msg.params)
	case "thread/tokenUsage/updated":
		return m.applyTokenUsage(msg.params)
	case "thread/name/updated":
		return m.applyThreadName(msg.params)
	default:
		m.noticef("files changed by later turns are not reverted")
		return nil
	}
}

func (m *model) applyTurnNotification(msg notificationMsg) (tea.Cmd, error) {
	switch msg.method {
	case "turn/started":
		return nil, m.applyTurnStarted(msg.params)
	case "turn/completed":
		return m.applyTurnCompleted(msg.params)
	case "dalgona/retry/wait":
		return nil, m.applyRetryWait(msg.params)
	default:
		return nil, m.applyPlan(msg.params)
	}
}

func (m *model) applyItemNotification(msg notificationMsg) error {
	switch msg.method {
	case "item/fileChange/patchUpdated":
		return m.applyPatchUpdated(msg.params)
	case "item/started":
		return m.applyItemStarted(msg.params)
	case "item/completed":
		return m.applyItemCompleted(msg.params)
	default:
		return m.applyDelta(msg.method, msg.params)
	}
}

func (m *model) applyThreadStarted(params jsontext.Value) error {
	event, err := decodeParams[rpc.ThreadStartedNotification](params)
	if err != nil {
		return err
	}
	if !m.acceptThread(event.Thread.ID) {
		return nil
	}
	m.thread.title = threadTitle(event.Thread)
	m.setState(threadState(event.Thread.Status))
	return nil
}

func (m *model) applyThreadStatus(params jsontext.Value) error {
	event, err := decodeParams[rpc.ThreadStatusChangedNotification](params)
	if err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) || event.Seq < m.thread.seq {
		return nil
	}
	m.thread.seq = max(m.thread.seq, event.Seq)
	state := threadState(event.Status)
	if state == stateIdle && endedTurnState(m.thread.state) {
		return nil
	}
	m.setState(state)
	return nil
}

func (m *model) applyThreadName(params jsontext.Value) error {
	event, err := decodeParams[rpc.ThreadNameUpdatedNotification](params)
	if err != nil {
		return err
	}
	if m.acceptThread(event.ThreadID) {
		m.thread.title = sanitizeText(cmp.Or(event.ThreadName, event.ThreadID))
	}
	return nil
}

func (m *model) applyChildSpawned(params jsontext.Value) error {
	event, err := decodeParams[rpc.ChildSpawnedNotification](params)
	if err != nil {
		return err
	}
	if m.acceptThread(event.ThreadID) {
		m.attachChild(sanitizeText(event.ChildID))
		if m.conn.opts.OnChildSpawned != nil {
			m.conn.opts.OnChildSpawned(event.ChildID, event.Cwd)
		}
	}
	return nil
}

func (m *model) applyAdvisorNote(params jsontext.Value) error {
	event, err := decodeParams[rpc.AdvisorNoteNotification](params)
	if err != nil {
		return err
	}
	if m.acceptThread(event.ThreadID) {
		m.out.seq++
		key := fmt.Sprintf("advisor:%s:%d", event.ThreadID, m.out.seq)
		m.commit(key, strings.Split(m.renderAdvisorCard(event.Advisor, event.Severity, event.Note), "\n"))
	}
	return nil
}

type judgedReport struct {
	threadID    string
	turnID      string
	targetID    string
	verdict     string
	summary     string
	paths       []string
	attribution reviewAttribution
}

func (m *model) applyJudged(r judgedReport) {
	if !m.acceptThread(r.threadID) {
		return
	}
	key := "review:" + r.threadID + ":" + r.turnID
	if r.targetID != "" {
		key += ":" + r.targetID
	}
	m.printReview(key, r.verdict, r.summary, r.paths, r.attribution)
}

func (m *model) applyToolJudged(params jsontext.Value) error {
	e, err := decodeParams[rpc.ToolJudgedNotification](params)
	if err != nil {
		return err
	}
	m.applyJudged(judgedReport{e.ThreadID, e.TurnID, e.ItemID, e.Verdict, e.Summary, e.Paths, reviewAttribution{e.By, e.Fallback, e.Judges}})
	return nil
}

func (m *model) applyTurnJudged(params jsontext.Value) error {
	e, err := decodeParams[rpc.TurnJudgedNotification](params)
	if err != nil {
		return err
	}
	m.applyJudged(judgedReport{e.ThreadID, e.TurnID, "", e.Verdict, e.Summary, e.Paths, reviewAttribution{e.By, e.Fallback, e.Judges}})
	return nil
}

func (m *model) applyLoginCompleted(params jsontext.Value) error {
	event, err := decodeParams[rpc.AccountLoginCompletedNotification](params)
	if err != nil {
		return err
	}
	m.closeWaitingLogin()
	if !event.Success {
		m.loginFailed(cmp.Or(event.Error, "unknown error"))
		return nil
	}
	m.noticef("Signed in to %s. Run /model to choose a model", event.Provider)
	return nil
}

func (m *model) applyTurnEffort(params jsontext.Value) error {
	e, err := decodeParams[rpc.TurnEffortNotification](params)
	if err != nil {
		return err
	}
	if e.ThreadID == m.thread.id && m.thread.turnID != "" && e.TurnID == m.thread.turnID {
		m.thread.turnEffort = &e.Effort
		m.thread.turnRevision++
	}
	return nil
}

func (m *model) applyTurnStarted(params jsontext.Value) error {
	event, err := decodeParams[rpc.TurnStartedNotification](params)
	if err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) {
		return nil
	}
	keep := !m.thread.turnStartedAt.IsZero() && (m.thread.turnSent || event.Turn.ID == m.thread.turnID)
	m.thread.turnID = event.Turn.ID
	m.thread.turnEffort = nil
	m.thread.turnRevision++
	if !keep {
		m.thread.turnStartedAt = m.now()
	}
	m.thread.turnSent = false
	m.noticef("")
	m.live.planN = 0
	m.setState(stateRunning)
	return nil
}

func (m *model) applyTurnCompleted(params jsontext.Value) (tea.Cmd, error) {
	event, err := decodeParams[rpc.TurnCompletedNotification](params)
	if err != nil {
		return nil, err
	}
	if !m.acceptThread(event.ThreadID) {
		return nil, nil
	}
	if m.thread.turnID != "" && event.Turn.ID != m.thread.turnID {
		return nil, nil
	}
	m.flushGroup()
	m.thread.turnID = ""
	m.thread.turnEffort = nil
	m.thread.turnRevision++
	if m.thread.completedTurns == nil {
		m.thread.completedTurns = map[string]struct{}{}
	}
	m.thread.completedTurns[event.Turn.ID] = struct{}{}
	m.thread.lastTurn = time.Duration(event.Turn.DurationMs) * time.Millisecond
	if m.thread.lastTurn == 0 && !m.thread.turnStartedAt.IsZero() {
		m.thread.lastTurn = m.now().Sub(m.thread.turnStartedAt)
	}
	m.thread.turnSent = false
	m.live.plan = nil
	m.live.retryUntil = time.Time{}
	m.setState(turnState(event.Turn.Status))
	notify := m.notify(m.notifyLine(turnWord(event.Turn.Status)))
	if m.thread.state == stateInterrupted {
		m.noticef("")
	}
	return m.turnDoneCmd(notify), nil
}

func (m *model) turnDoneCmd(notify tea.Cmd) tea.Cmd {
	if m.thread.state != stateIdle {
		if len(m.queue) > 0 {
			m.noticef("Held · %d · Enter on an empty prompt sends the next", len(m.queue))
		}
		return notify
	}
	return tea.Batch(notify, m.startQueuedFollowup())
}

func (m *model) applyItemStarted(params jsontext.Value) error {
	event, err := decodeParams[rpc.ItemStartedNotification](params)
	if err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) {
		return nil
	}
	live := m.liveItem(event.Item.ID)
	live.Item = mergeItem(sanitizeItem(event.Item), live.Item)
	if event.Item.Type == "agentMessage" {
		m.flushGroup()
	}
	return nil
}

func (m *model) applyItemCompleted(params jsontext.Value) error {
	event, err := decodeParams[rpc.ItemCompletedNotification](params)
	if err != nil {
		return err
	}
	m.forgetPatch(patchKey{event.ThreadID, event.Item.ID})
	if !m.acceptThread(event.ThreadID) {
		return nil
	}
	final := sanitizeItem(event.Item)
	if final.Type == "userMessage" {
		m.input.history.add(final.Text, m.thread.turnID)
	}
	live := m.liveItem(event.Item.ID)
	answer := cmp.Or(final.Type, live.Type) == "agentMessage"
	if answer {
		m.flushGroup()
		m.finishAnswer(event.ThreadID, live, final.Text)
	}
	children := live.children
	item := live.settle(final)
	m.live.items = slices.DeleteFunc(m.live.items, func(candidate liveItem) bool { return candidate.ID == item.ID })
	m.rebuildLiveIndex()
	if !answer && !isAdvisorItem(item) {
		m.settleItem(itemKey(event.ThreadID, event.TurnID, item.ID), item, children)
	}
	return nil
}

func (m *model) liveItem(id string) *liveItem {
	if m.live.byID == nil {
		m.rebuildLiveIndex()
	}
	if index, ok := m.live.byID[id]; ok {
		return &m.live.items[index]
	}
	var item liveItem
	item.ID = id
	m.live.items = append(m.live.items, item)
	m.live.byID[id] = len(m.live.items) - 1
	return &m.live.items[len(m.live.items)-1]
}

func (m *model) rebuildLiveIndex() {
	m.live.byID = make(map[string]int, len(m.live.items))
	for i := range m.live.items {
		m.live.byID[m.live.items[i].ID] = i
	}
}

func mergeItem(item, live rpc.Item) rpc.Item {
	item.Text = cmp.Or(item.Text, live.Text)
	item.Output = cmp.Or(item.Output, live.Output)
	item.Command = cmp.Or(item.Command, live.Command)
	item.Type = cmp.Or(item.Type, live.Type)
	return item
}

func (m *model) applyDelta(method string, params jsontext.Value) error {
	delta := m.live.deltas
	if err := delta.decode(params); err != nil {
		return err
	}
	if !m.acceptThread(delta.threadID) {
		return nil
	}
	if m.thread.turnID != "" && delta.turnID != "" && delta.turnID != m.thread.turnID {
		return nil
	}
	item := m.liveItem(delta.itemID)
	item.Type = deltaType(method)
	if item.Type == "agentMessage" {
		m.flushGroup()
		m.streamAnswer(delta.threadID, item, delta.text)
		return nil
	}
	frag := appendSanitized(nil, delta.text)
	if item.Type == "commandExecution" {
		item.output = append(item.output, frag...)
	} else {
		item.text = append(item.text, frag...)
	}
	item.tailMark = len(item.streamed())
	item.noteTail(frag)
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
	event, err := decodeParams[rpc.TurnPlanUpdatedNotification](params)
	if err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) {
		return nil
	}
	if event.TurnID != m.live.planTurn {
		m.live.planTurn = event.TurnID
		m.live.planN = 0
	}
	m.live.plan = event.Plan
	m.commit(fmt.Sprintf("plan:%s:%d", event.TurnID, m.live.planN), planLines(event.Plan))
	m.live.planN++
	return nil
}

func (m *model) applyRetryWait(params jsontext.Value) error {
	event, err := decodeParams[rpc.RetryWaitNotification](params)
	if err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) {
		return nil
	}
	if m.thread.turnID != "" && event.TurnID != "" && event.TurnID != m.thread.turnID {
		return nil
	}
	m.live.retryUntil = time.Time{}
	if event.RemainingMS > 0 {
		m.live.retryUntil = m.now().Add(time.Duration(event.RemainingMS) * time.Millisecond)
	}
	return nil
}

func planStatus(status string) string {
	switch status {
	case "completed":
		return "Done"
	case "inProgress":
		return "Working"
	}
	return "Pending"
}

const planVisibleSteps = 5

func planLines(plan []rpc.TurnPlanStep) []string {
	lines := []string{"Plan"}
	for i, step := range plan {
		if i == planVisibleSteps {
			return append(lines, fmt.Sprintf("+%d more", len(plan)-planVisibleSteps))
		}
		lines = append(lines, planStatus(step.Status)+" "+firstLine(sanitizeText(step.Step)))
	}
	return lines
}

func (m *model) applyError(params jsontext.Value) error {
	event, err := decodeParams[rpc.ErrorNotification](params)
	if err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) {
		return nil
	}
	if event.WillRetry {
		m.noticef("error: %s; retrying the turn, no action needed", event.Error.Message)
		return nil
	}
	m.setState(stateFailed)
	m.errorf("error: %s; retry the turn, %s, or /new", event.Error.Message, m.resumeHint())
	return nil
}

func (m *model) acceptThread(id string) bool {
	if m.thread.id == "" {
		m.setThread(id)
	}
	return id == m.thread.id
}

func (m *model) setThread(id string) {
	if id == m.thread.id {
		return
	}
	m.flushGroup()
	m.thread.id = id
	m.thread.turnID = ""
	m.thread.turnEffort = nil
	m.thread.turnRevision++
	m.thread.seq = 0
	m.thread.completedTurns = nil
	m.thread.lastTurn = 0
	m.thread.turnStartedAt = time.Time{}
	m.thread.turnSent = false
	m.live.plan = nil
	m.live.retryUntil = time.Time{}
	m.forgetUnshownPatches()
	m.thread.effort = ""
	m.thread.usage = rpc.ThreadTokenUsage{}
}

func (m *model) applyTokenUsage(params jsontext.Value) error {
	event, err := decodeParams[rpc.ThreadTokenUsageUpdatedNotification](params)
	if err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) {
		return nil
	}
	m.thread.usage = event.TokenUsage
	return nil
}

func (m *model) noticeMoved(title string) {
	if n := len(m.queue); n > 0 {
		m.noticef("%d follow-ups moved to %s; press Enter on an empty prompt to send the next", n, title)
	}
}

func (m *model) protocolError(method string, err error) {
	m.errorf("protocol error in %s: %v; reconnect with %s or use /new", method, err, m.resumeHint())
}

func (m *model) loginFailed(cause string) {
	m.errorf("Login failed: %s; run /login again", cause)
}

func (m *model) resumeHint() string {
	if m.thread.id == "" {
		return "/resume"
	}
	return "/resume " + m.thread.id
}

func threadTitle(thread rpc.Thread) string {
	if thread.Name == "" {
		return sanitizeText(thread.ID)
	}
	return sanitizeText(thread.Name)
}

func endedTurnState(state string) bool {
	return state == stateInterrupted || state == stateFailed
}

func threadState(status rpc.ThreadStatus) string {
	switch status.Type {
	case "active":
		if slices.Contains(status.ActiveFlags, "waitingOnApproval") {
			return stateAwaitingApproval
		}
		if slices.Contains(status.ActiveFlags, "waitingOnUserInput") {
			return stateAwaitingInput
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

func turnWord(status string) string {
	switch status {
	case "failed":
		return "Failed"
	case "interrupted":
		return "Stopped"
	default:
		return "Done"
	}
}

func (m *model) setState(state string) {
	if m.thread.state != stateDisconnected {
		m.thread.state = state
	}
	m.emitStatus()
}

func (m *model) emitStatus() {
	if m.conn.opts.OnStatus != nil {
		m.conn.opts.OnStatus(Status{State: m.thread.state, Seq: m.thread.seq, ThreadID: m.thread.id})
	}
}
