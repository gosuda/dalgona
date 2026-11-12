package tui

import (
	"cmp"
	"encoding/json/jsontext"
	"fmt"
	"slices"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func (m *model) applyNotification(msg notificationMsg) tea.Cmd {
	command, err := m.applyDecodedNotification(msg)
	if err != nil {
		m.errorf("protocol error in %s: %v; reconnect with %s or use /new", msg.method, err, m.resumeHint())
		return nil
	}
	m.emitStatus()
	return command
}

func (m *model) applyDecodedNotification(msg notificationMsg) (tea.Cmd, error) {
	switch msg.method {
	case "thread/started", "thread/status/changed", "thread/name/updated", "thread/reverted", "thread/tokenUsage/updated", "serverRequest/resolved":
		return nil, m.applyThreadNotification(msg)
	case "turn/started", "turn/completed", "turn/plan/updated", "dalgona/retry/wait":
		return m.applyTurnNotification(msg)
	case "item/started", "item/completed", "item/agentMessage/delta", "item/reasoning/textDelta", "item/commandExecution/outputDelta", "item/fileChange/patchUpdated":
		return nil, m.applyItemNotification(msg)
	case "error":
		return nil, m.applyError(msg.params)
	case "dalgona/thread/childSpawned":
		return nil, m.applyChildSpawned(msg.params)
	case "dalgona/advisor/note", "dalgona/tool/judged", "dalgona/turn/judged":
		return nil, m.applyDalgonaNotification(msg)
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
	var event rpc.ThreadStartedNotification
	if err := decodeParams(params, &event); err != nil {
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
	var event rpc.ThreadStatusChangedNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) || event.Seq > 0 && event.Seq < m.thread.seq {
		return nil
	}
	m.thread.seq = max(m.thread.seq, event.Seq)
	m.setState(threadState(event.Status))
	return nil
}

func (m *model) applyThreadName(params jsontext.Value) error {
	var event rpc.ThreadNameUpdatedNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if m.acceptThread(event.ThreadID) {
		m.thread.title = sanitizeText(cmp.Or(event.ThreadName, event.ThreadID))
	}
	return nil
}

func (m *model) applyChildSpawned(params jsontext.Value) error {
	var event rpc.ChildSpawnedNotification
	if err := decodeParams(params, &event); err != nil {
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
	var event rpc.AdvisorNoteNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if m.acceptThread(event.ThreadID) {
		m.out.seq++
		key := fmt.Sprintf("advisor:%s:%d", event.ThreadID, m.out.seq)
		m.commit(key, strings.Split(m.renderAdvisorCard(event.Advisor, event.Severity, event.Note), "\n"))
	}
	return nil
}

func (m *model) applyToolJudged(params jsontext.Value) error {
	var event rpc.ToolJudgedNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if m.acceptThread(event.ThreadID) {
		m.printReview("review:"+event.ThreadID+":"+event.ItemID, event.Verdict, event.Summary, event.Paths)
	}
	return nil
}

func (m *model) applyTurnJudged(params jsontext.Value) error {
	var event rpc.TurnJudgedNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if m.acceptThread(event.ThreadID) {
		m.printReview("review:"+event.ThreadID+":"+event.TurnID, event.Verdict, event.Summary, event.Paths)
	}
	return nil
}

func (m *model) applyLoginCompleted(params jsontext.Value) error {
	var event rpc.AccountLoginCompletedNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	m.closeWaitingLogin()
	if !event.Success {
		m.errorf("Login failed: %s; run /login again", cmp.Or(event.Error, "unknown error"))
		return nil
	}
	m.noticef("Signed in to %s. Run /model to choose a model", event.Provider)
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
	m.thread.turnID = event.Turn.ID
	m.thread.turnStartedAt = m.now()
	if event.Turn.StartedAt != 0 {
		m.thread.turnStartedAt = time.Unix(event.Turn.StartedAt, 0)
	}
	m.noticef("")
	m.live.planN = 0
	m.setState(stateRunning)
	return nil
}

func (m *model) applyTurnCompleted(params jsontext.Value) (tea.Cmd, error) {
	var event rpc.TurnCompletedNotification
	if err := decodeParams(params, &event); err != nil {
		return nil, err
	}
	if !m.acceptThread(event.ThreadID) {
		return nil, nil
	}
	m.flushGroup()
	m.thread.turnID = ""
	if m.thread.completedTurns == nil {
		m.thread.completedTurns = map[string]struct{}{}
	}
	m.thread.completedTurns[event.Turn.ID] = struct{}{}
	m.thread.lastTurn = time.Duration(event.Turn.DurationMs) * time.Millisecond
	if m.thread.lastTurn == 0 && !m.thread.turnStartedAt.IsZero() {
		m.thread.lastTurn = m.now().Sub(m.thread.turnStartedAt)
	}
	m.live.plan = nil
	m.live.retryUntil = time.Time{}
	m.setState(turnState(event.Turn.Status))
	notify := m.notify(m.notifyLine(turnWord(event.Turn.Status)))
	if m.thread.state != stateIdle {
		if len(m.queue) > 0 {
			m.noticef("Held · %d · Enter on an empty prompt sends the next", len(m.queue))
		}
		return notify, nil
	}
	return tea.Batch(notify, m.startQueuedFollowup()), nil
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
	live.Item = mergeItem(sanitizeItem(event.Item), live.Item)
	if event.Item.Type == "agentMessage" {
		m.flushGroup()
	}
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
	if !answer && !isAdvisorItem(item) {
		m.settleItem("item:"+event.ThreadID+":"+item.ID, item, children)
	}
	return nil
}

func (m *model) liveItem(id string) *liveItem {
	index := slices.IndexFunc(m.live.items, func(item liveItem) bool { return item.ID == id })
	if index < 0 {
		m.live.items = append(m.live.items, liveItem{Item: rpc.Item{ID: id}})
		index = len(m.live.items) - 1
	}
	return &m.live.items[index]
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
	item := m.liveItem(delta.itemID)
	item.Type = deltaType(method)
	if item.Type == "agentMessage" {
		m.flushGroup()
		m.streamAnswer(delta.threadID, item, delta.text)
		return nil
	}
	if item.Type == "commandExecution" {
		item.output = appendSanitized(item.output, delta.text)
	} else {
		item.text = appendSanitized(item.text, delta.text)
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
	var event rpc.RetryWaitNotification
	if err := decodeParams(params, &event); err != nil {
		return err
	}
	if !m.acceptThread(event.ThreadID) {
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
	var event rpc.ErrorNotification
	if err := decodeParams(params, &event); err != nil {
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
	m.thread.seq = 0
	m.thread.completedTurns = nil
	m.thread.lastTurn = 0
	m.thread.turnStartedAt = time.Time{}
	m.live.plan = nil
	m.live.retryUntil = time.Time{}
	m.thread.effort = ""
	m.thread.usage = rpc.ThreadTokenUsage{}
}

func (m *model) applyTokenUsage(params jsontext.Value) error {
	var event rpc.ThreadTokenUsageUpdatedNotification
	if err := decodeParams(params, &event); err != nil {
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
