package tui

import (
	"cmp"
	"fmt"
	"slices"
	"strconv"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	listHint  = "↑/↓ choose · Enter select · Esc cancel"
	modelHint = "↑/↓ choose · Tab effort · Enter select · Esc cancel"
	queueHint = "↑/↓ move · Esc close"
)

func (m *model) openList(list *listPanel) {
	m.closeList()
	m.list = list
}

func (m *model) closeList() {
	if m.list != nil {
		m.list.stopResumeSearch()
	}
	m.list = nil
	m.input.field = textField{}
}

const queueKind = "queue"

func (m *model) openQueue() {
	m.openList(&listPanel{kind: queueKind, title: "Queued follow-ups", items: queueItems(m.queue)})
}

func queueItems(queue []followup) []listItem {
	items := make([]listItem, 0, len(queue))
	for i, item := range queue {
		first, _, _ := strings.Cut(item.text, "\n")
		items = append(items, listItem{id: fmt.Sprint(i), label: fmt.Sprintf("%d. %s", i+1, sanitizeText(first))})
	}
	return items
}

func (m *model) syncQueue() {
	if m.list == nil || m.list.kind != queueKind {
		return
	}
	m.list.items = queueItems(m.queue)
	m.list.cursor = min(m.list.cursor, max(0, len(m.list.items)-1))
}

func (m *model) queueKey(key string) {
	if key == "esc" {
		m.closeList()
		return
	}
	m.listMove(key)
}

func (m *model) listRows() int {
	return max(1, panelRows(panelList, m.screen.height)-2-len(m.resumePreview()))
}

func (m *model) listPanelView() panelView {
	list := m.list
	items := list.visible(m.listQuery())
	titleCells := 0
	if list.kind == "resume" {
		titleCells = m.resumeTitleCells(items)
	}
	body := make([]string, 0, len(items))
	for pos, item := range items {
		body = append(body, m.listLine(item, pos == list.cursor, titleCells))
	}
	cursor := list.cursor
	if len(list.items) == 0 && list.empty != "" {
		body, cursor = []string{list.empty}, -1
	}
	return panelView{kind: list.kind, layout: panelList, title: list.title, body: body, preview: m.resumePreview(), actions: []string{list.hint()}, cursor: cursor, scroll: list.scroll}
}

func (p *listPanel) hint() string {
	if p.err != "" {
		return p.err
	}
	switch p.kind {
	case "model":
		return modelHint
	case "resume":
		return p.resumeHint()
	case queueKind:
		return queueHint
	case historyKind:
		return historyHint
	}
	return listHint
}

func (m *model) listLine(item listItem, selected bool, titleCells int) string {
	if m.list != nil && m.list.kind == "resume" {
		return m.resumeLine(item, selected, titleCells)
	}
	rowTheme := m.screen.dockTheme()
	label := rowTheme.paint(roleText, item.label)
	if selected {
		label = rowTheme.paintBold(roleText, item.label)
	}
	line := label
	if item.active {
		line += rowTheme.paint(roleDim, " (active)")
	}
	if item.detail != "" {
		line += rowTheme.paint(roleDim, " — "+item.detail)
	}
	if pick, ok := m.list.efforts[item.id]; ok {
		line += rowTheme.paint(roleDim, "  effort "+pick.current)
	}
	return line
}

func (m *model) resumeLine(item listItem, selected bool, titleCells int) string {
	title := item.label
	if item.active {
		title += " (active)"
	}
	rowTheme := m.screen.dockTheme()
	head := rowTheme.paint(roleText, m.screen.pad(m.screen.fit(title, titleCells), titleCells))
	if selected {
		head = rowTheme.paintBold(roleText, m.screen.pad(m.screen.fit(title, titleCells), titleCells))
	}
	line := head
	if item.date != "" {
		line += "  " + rowTheme.paint(roleDim, m.screen.padLeft(item.date, resumeDateCells))
	} else {
		line += "  " + strings.Repeat(" ", resumeDateCells)
	}
	if item.detail != "" {
		line += "  " + rowTheme.paint(roleDim, item.detail)
	}
	return line
}

func (m *model) resumeTitleCells(items []listItem) int {
	width := 0
	state := 0
	for _, item := range items {
		title := item.label
		if item.active {
			title += " (active)"
		}
		width = max(width, m.screen.cells(title))
		state = max(state, m.screen.cells(item.detail))
	}
	budget := panelRowWidth(&m.screen) - panelRailCells - 4 - resumeDateCells - 2 - state
	return min(width, max(1, budget))
}

func (m *model) highlighted() (listItem, bool) {
	items := m.list.visible(m.listQuery())
	if m.list.cursor < 0 || m.list.cursor >= len(items) {
		return listItem{}, false
	}
	return items[m.list.cursor], true
}

func (m *model) moveList(delta int) {
	count := len(m.list.visible(m.listQuery()))
	if count == 0 {
		m.list.cursor = 0
		return
	}
	m.list.cursor = (m.list.cursor + delta%count + count) % count
}

const listWheelRows = 3

func (m *model) wheelList(msg tea.MouseWheelMsg) {
	if msg.Button != tea.MouseWheelUp && msg.Button != tea.MouseWheelDown {
		return
	}
	delta := listWheelRows
	if msg.Button == tea.MouseWheelUp {
		delta = -listWheelRows
	}
	count := len(m.list.visible(m.listQuery()))
	if count == 0 {
		return
	}
	rows := m.listRows()
	start, _ := bodySpan(m.listPanelView(), rows)
	m.list.scroll = min(max(start+delta, 0), max(count-rows, 0))
	last := min(m.list.scroll+rows, count) - 1
	m.list.cursor = min(max(m.list.cursor, m.list.scroll), last)
}

func (m *model) listKey(msg tea.KeyPressMsg) tea.Cmd {
	if m.list.kind == queueKind {
		m.queueKey(msg.String())
		return nil
	}
	switch key := msg.String(); key {
	case "enter":
		return m.listEnter()
	case "esc":
		m.escapeList()
	case "tab":
		return m.listTab()
	case "shift+tab":
		m.cycleEffort(-1)
	case "delete", "backspace":
		m.listDelete(msg)
	default:
		if !m.listMove(key) {
			m.listEdit(msg)
		}
	}
	return nil
}

func (m *model) listTab() tea.Cmd {
	if search := m.list.search; search != nil && search.cancel == nil && (search.err != "" || search.nextCursor != "") {
		return m.fetchResumeMatches(search, 0)
	}
	m.cycleEffort(1)
	return nil
}

func (m *model) listDelete(msg tea.KeyPressMsg) {
	if m.list.kind == historyKind && (msg.String() == "delete" || m.input.field.value() == "") {
		m.removeHistory()
		return
	}
	m.listEdit(msg)
}

func (m *model) listEnter() tea.Cmd {
	if m.list.pending != "" {
		return m.sendReview()
	}
	return m.activateChoice()
}

func (m *model) listMove(key string) bool {
	switch key {
	case "up", "ctrl+p":
		m.moveList(-1)
	case "down", "ctrl+n":
		m.moveList(1)
	case "pgup":
		m.moveList(-m.listRows())
	case "pgdown":
		m.moveList(m.listRows())
	default:
		return false
	}
	return true
}

func (m *model) listEdit(msg tea.KeyPressMsg) {
	key := msg.String()
	if key == "left" || key == "right" {
		m.input.field.edit(key)
		return
	}
	if m.input.field.edit(key) || (msg.Text != "" && m.input.field.insert(msg.Text)) {
		m.list.cursor = 0
	}
}

func (m *model) escapeList() {
	if m.list.pending != "" {
		m.list.pending = ""
		m.list.err = ""
		m.input.field = textField{}
		return
	}
	if m.input.field.value() != "" {
		m.input.field = textField{}
		m.list.cursor = 0
		return
	}
	m.closeList()
}

func (m *model) cycleEffort(step int) {
	item, ok := m.highlighted()
	if !ok {
		return
	}
	pick, ok := m.list.efforts[item.id]
	if !ok {
		return
	}
	i := slices.Index(pick.levels, pick.current)
	if i < 0 {
		i = 0
	} else {
		i = (i + step + len(pick.levels)) % len(pick.levels)
	}
	pick.current = pick.levels[i]
	m.list.efforts[item.id] = pick
}

func (m *model) activateChoice() tea.Cmd {
	item, ok := m.highlighted()
	if !ok {
		return nil
	}
	kind := m.list.kind
	switch kind {
	case "model":
		return m.saveModel(item)
	case "advisor":
		return m.toggleAdvisor(item)
	case "review":
		return m.chooseReview(item)
	case historyKind:
		return m.loadHistory(item)
	}
	m.closeList()
	return m.activateClosed(kind, item)
}

func (m *model) activateClosed(kind string, item listItem) tea.Cmd {
	if busyThreadCommands[kind] && m.busyRefusal(kind) {
		return nil
	}
	switch kind {
	case "resume":
		return m.resumeThread(item.id)
	case "tree":
		params := rpc.ThreadLeafSetParams{ThreadID: m.thread.id, TurnID: item.id}
		return m.call("leaf", "dalgona/thread/leaf/set", params, nil, "")
	case "revert":
		m.openRevert(item.id, item.label)
	case "rewind":
		m.openRewind(item.id, item.label)
	case "fork":
		params := rpc.ThreadForkParams{ThreadID: m.thread.id, LastTurnID: item.id}
		return m.callSeq(m.resetPendingResume(""), "fork", "thread/fork", params, new(rpc.ThreadForkResponse))
	}
	return nil
}

func (m *model) saveModel(item listItem) tea.Cmd {
	m.list.err = ""
	m.list.pendingEffort = ""
	if pick, ok := m.list.efforts[item.id]; ok && pick.current != pick.base {
		m.list.pendingEffort = pick.current
	}
	return m.setModel(item.id)
}

func (m *model) applyModelResult(msg resultMsg) tea.Cmd {
	if msg.op == "modelEffort" {
		m.thread.effort = msg.text
		m.finishModelPick()
		m.noticef("model set to %s at effort %s", m.thread.modelName, msg.text)
		return nil
	}
	m.thread.modelName = msg.text
	if m.list != nil && m.list.kind == "model" && m.list.pendingEffort != "" {
		effort := m.list.pendingEffort
		m.list.pendingEffort = ""
		params := rpc.ConfigValueWriteParams{KeyPath: "model_reasoning_effort", MergeStrategy: "replace", Value: effort}
		return m.call("modelEffort", "config/value/write", params, nil, effort)
	}
	m.finishModelPick()
	m.noticef("%s", "model set to "+msg.text)
	return nil
}

func (m *model) finishModelPick() {
	if m.list != nil && m.list.kind == "model" {
		m.closeList()
	}
}

func (m *model) saveFailed(msg resultMsg) bool {
	if msg.op != "modelSet" && msg.op != "modelEffort" || m.list == nil || m.list.kind != "model" {
		return false
	}
	m.list.err = sanitizeText(fmt.Sprintf("Could not save: %v", msg.err))
	m.list.pendingEffort = ""
	return true
}

func (m *model) openThreadList(response *rpc.ThreadListResponse) {
	now := m.now()
	items := make([]listItem, 0, len(response.Data))
	for _, item := range response.Data {
		items = append(items, listItem{id: sanitizeText(item.ID), label: threadTitle(item), date: threadDate(item, now), detail: threadState(item.Status), active: item.ID == m.thread.id})
	}
	m.openList(&listPanel{kind: "resume", title: "resume a thread", items: items, openedAt: now})
}

func threadDate(thread rpc.Thread, now time.Time) string {
	seconds := thread.UpdatedAt
	if seconds <= 0 {
		seconds = thread.CreatedAt
	}
	if seconds <= 0 {
		return ""
	}
	return formatResumeDate(time.Unix(seconds, 0), now)
}

func (m *model) staleListReply(msg resultMsg) bool {
	if msg.seq != 0 && msg.seq != m.listSeq {
		return true
	}
	return m.panelOpen() || m.review != nil
}

func (m *model) applyListReply(msg resultMsg) tea.Cmd {
	if m.staleListReply(msg) {
		return nil
	}
	switch msg.op {
	case "models":
		return m.applyModels(msg.value)
	case "threads":
		return m.applyThreads(msg.value)
	}
	return m.applyTree(msg.op, msg.value)
}

func (m *model) applyModels(value any) tea.Cmd {
	response, ok := value.(*rpc.ModelListResponse)
	if !ok {
		return m.unexpectedResult("models")
	}
	m.rememberLevels(response.Data)
	list := newModelList(response.Data, m.thread.modelName, m.thread.effort)
	if len(list.items) == 0 {
		m.noticef("no models are available; run /login <provider> and retry /model")
		return nil
	}
	m.openList(list)
	return nil
}

func (m *model) applyThreads(value any) tea.Cmd {
	response, ok := value.(*rpc.ThreadListResponse)
	if !ok {
		return m.unexpectedResult("threads")
	}
	if len(response.Data) == 0 {
		m.noticef("no threads are available; use /new to start one")
		return nil
	}
	m.openThreadList(response)
	return nil
}

func (m *model) applyTree(op string, value any) tea.Cmd {
	response, ok := value.(*rpc.ThreadTreeResponse)
	if !ok {
		return m.unexpectedResult("thread tree")
	}
	kind, title := treeKind(op)
	items := make([]listItem, 0, len(response.Turns))
	for _, turn := range response.Turns {
		if op != "tree" && !turn.Active {
			continue
		}
		items = append(items, listItem{id: sanitizeText(turn.TurnID), label: sanitizeText(turn.Preview), detail: sanitizeText(turn.TurnID), active: turn.Active})
	}
	if op == "forkTree" || op == "rewindTree" {
		slices.Reverse(items)
	}
	if len(items) == 0 {
		m.noticef("no turns are available on this path; send a prompt first")
		return nil
	}
	m.openList(&listPanel{kind: kind, title: title, items: items, efforts: map[string]effortPick{}})
	return nil
}

func treeKind(op string) (kind, title string) {
	switch op {
	case "revertTree":
		return "revert", "revert before a turn"
	case "forkTree":
		return "fork", "fork at a turn"
	case "rewindTree":
		return "rewind", "rewind to before a turn"
	}
	return "tree", "choose the active branch"
}

func (m *model) rewindCommand() tea.Cmd {
	return m.treeCommand("rewindTree")
}

func newModelList(models []rpc.Model, activeModel, activeEffort string) *listPanel {
	list := &listPanel{kind: "model", title: "choose a model", efforts: map[string]effortPick{}}
	for _, role := range []string{"default", "smol", "slow"} {
		for _, item := range models {
			if !item.Hidden && slices.Contains(item.Presets, role) {
				list.items = append(list.items, list.modelItem(item, role+" · "+sanitizeText(item.ID), activeModel, activeEffort))
			}
		}
	}
	for _, item := range models {
		if !item.Hidden {
			list.items = append(list.items, list.modelItem(item, sanitizeText(item.ID), activeModel, activeEffort))
		}
	}
	return list
}

func (p *listPanel) modelItem(item rpc.Model, label, activeModel, activeEffort string) listItem {
	id := sanitizeText(item.ID)
	if _, ok := p.efforts[id]; !ok {
		levels := make([]string, 0, len(item.SupportedReasoningEfforts))
		for _, option := range item.SupportedReasoningEfforts {
			levels = append(levels, option.ReasoningEffort)
		}
		if len(levels) >= 2 {
			base := item.DefaultReasoningEffort
			if id == activeModel && slices.Contains(levels, activeEffort) {
				base = activeEffort
			}
			if !slices.Contains(levels, base) {
				base = levels[0]
			}
			p.efforts[id] = effortPick{levels: levels, base: base, current: base}
		}
	}
	return listItem{id: id, label: label, detail: sanitizeText(item.DisplayName), active: id == activeModel}
}

func (m *model) listQuery() string {
	if m.list.pending != "" {
		return ""
	}
	return m.input.field.value()
}

func (m *model) openAdvisorList(states []rpc.AdvisorState) {
	items := make([]listItem, 0, len(states))
	for _, state := range states {
		items = append(items, listItem{id: sanitizeText(state.Name), label: sanitizeText(state.Name) + " · " + sanitizeText(state.Status), detail: advisorDetail(state), active: state.Status != "disabled"})
	}
	m.openList(&listPanel{kind: "advisor", title: "advisor", items: items, efforts: map[string]effortPick{}})
}

func advisorDetail(state rpc.AdvisorState) string {
	return fmt.Sprintf("%s · %d notes · %d in / %d out tokens", cmp.Or(sanitizeText(state.Model), "default model"), state.Notes, state.Usage.InputTokens, state.Usage.OutputTokens)
}

func (m *model) toggleAdvisor(item listItem) tea.Cmd {
	m.list.err = ""
	enabled := !item.active
	params := rpc.DalgonaAdvisorSetParams{ThreadID: m.thread.id, Enabled: enabled}
	return m.call("advisorSet", "dalgona/advisor/set", params, nil, strconv.FormatBool(enabled))
}

func (m *model) chooseReview(item listItem) tea.Cmd {
	if item.id == reviewUncommitted {
		m.closeList()
		return m.reviewStart(reviewUncommitted, "")
	}
	m.list.pending = item.id
	m.list.err = ""
	m.input.field = textField{}
	return nil
}

func (m *model) sendReview() tea.Cmd {
	m.list.err = ""
	return m.reviewStart(m.list.pending, m.input.field.value())
}

func reviewLabel(target string) string {
	switch target {
	case reviewBase:
		return "branch"
	case reviewCommit:
		return "commit"
	}
	return "instructions"
}

func (m *model) reviewFailed(msg resultMsg) bool {
	if msg.op != "review" || m.list == nil || m.list.kind != "review" {
		return false
	}
	m.list.err = sanitizeText(fmt.Sprintf("%v", msg.err))
	return true
}

func (m *model) finishReview() {
	if m.list != nil && m.list.kind == "review" {
		m.closeList()
	}
}

func (m *model) finishAdvisorList() {
	if m.list != nil && m.list.kind == "advisor" {
		m.closeList()
	}
}

const (
	historyKind       = "history"
	historyHint       = "type to filter · Enter load · Delete remove · Esc close"
	historyEmpty      = "No prompt history yet."
	historyLabelBytes = 1024
)

func (m *model) openHistorySearch() {
	if m.input.mode != inputPrompt {
		return
	}
	m.openList(&listPanel{kind: historyKind, title: "history", empty: historyEmpty})
	m.rebuildHistory()
}

func historyItems(entries []historyEntry) []listItem {
	items := make([]listItem, 0, len(entries))
	for _, entry := range slices.Backward(entries) {
		items = append(items, listItem{id: strconv.FormatUint(entry.id, 10), label: historyLabel(entry.text)})
	}
	return items
}

func historyLabel(text string) string {
	head := strings.ToValidUTF8(text[:min(len(text), historyLabelBytes)], "")
	return strings.Join(strings.Fields(head), " ")
}

func historyID(item listItem) uint64 {
	id, err := strconv.ParseUint(item.id, 10, 64)
	if err != nil {
		return 0
	}
	return id
}

func (m *model) rebuildHistory() {
	list := m.list
	held, had := m.highlighted()
	list.items = historyItems(m.input.history.allEntries())
	list.revision = m.input.history.revision
	items := list.visible(m.listQuery())
	at := slices.IndexFunc(items, func(item listItem) bool { return had && item.id == held.id })
	if at < 0 {
		at = min(list.cursor, max(0, len(items)-1))
	}
	list.cursor = at
}

func (m *model) syncHistory() {
	if m.list != nil && m.list.kind == historyKind && m.list.revision != m.input.history.revision {
		m.rebuildHistory()
	}
}

func (m *model) loadHistory(item listItem) tea.Cmd {
	entry, ok := m.input.history.load(historyID(item))
	if !ok {
		m.rebuildHistory()
		return nil
	}
	m.input.composer.recall(entry.text, entry.verbatim)
	m.closeList()
	return nil
}

func (m *model) removeHistory() {
	item, ok := m.highlighted()
	if !ok {
		return
	}
	if index, found := m.input.history.indexOf(historyID(item)); found {
		m.input.history.remove(index)
	}
	m.rebuildHistory()
}
