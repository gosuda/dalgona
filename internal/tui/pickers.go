package tui

import (
	"fmt"
	"slices"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

const (
	listHint  = "↑/↓ choose · Enter select · Esc cancel"
	modelHint = "↑/↓ choose · Tab effort · Enter select · Esc cancel"
)

func (m *model) openList(list *listPanel) {
	m.list = list
	m.input.field = textField{}
}

func (m *model) closeList() {
	m.list = nil
	m.input.field = textField{}
}

func (m *model) listRows() int {
	return max(1, panelRows(panelList, m.screen.height)-2)
}

func (m *model) listPanelView() panelView {
	list := m.list
	indexes := list.visible(m.input.field.value())
	body := make([]string, 0, len(indexes))
	for _, i := range indexes {
		body = append(body, m.listLine(list.items[i]))
	}
	actions := []string{listHint}
	if list.kind == "model" {
		actions[0] = modelHint
	}
	if list.err != "" {
		actions = []string{list.err}
	}
	return panelView{kind: list.kind, layout: panelList, title: list.title, body: body, actions: actions, cursor: list.cursor}
}

func (m *model) listLine(item listItem) string {
	line := item.label
	if item.active {
		line += " (active)"
	}
	if item.detail != "" {
		line += " — " + item.detail
	}
	if pick, ok := m.list.efforts[item.id]; ok {
		line += "  effort " + pick.current
	}
	return line
}

func (m *model) highlighted() (listItem, bool) {
	indexes := m.list.visible(m.input.field.value())
	if m.list.cursor < 0 || m.list.cursor >= len(indexes) {
		return listItem{}, false
	}
	return m.list.items[indexes[m.list.cursor]], true
}

func (m *model) moveList(delta int) {
	count := len(m.list.visible(m.input.field.value()))
	if count == 0 {
		m.list.cursor = 0
		return
	}
	m.list.cursor = (m.list.cursor + delta%count + count) % count
}

func (m *model) listKey(msg tea.KeyPressMsg) tea.Cmd {
	switch key := msg.String(); key {
	case "enter":
		return m.activateChoice()
	case "esc":
		m.escapeList()
	case "tab":
		m.cycleEffort(1)
	case "shift+tab":
		m.cycleEffort(-1)
	default:
		if !m.listMove(key) {
			m.listEdit(msg)
		}
	}
	return nil
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
	if kind == "model" {
		return m.saveModel(item)
	}
	m.closeList()
	switch kind {
	case "resume":
		return m.resumeThread(item.id)
	case "tree":
		params := rpc.ThreadLeafSetParams{ThreadID: m.thread.id, TurnID: item.id}
		return m.call("leaf", "dalgona/thread/leaf/set", params, nil, "")
	case "revert":
		m.openRevert(item.id, item.label)
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
	items := make([]listItem, 0, len(response.Data))
	for _, item := range response.Data {
		items = append(items, listItem{id: sanitizeText(item.ID), label: threadTitle(item), detail: threadState(item.Status), active: item.ID == m.thread.id})
	}
	m.openList(&listPanel{kind: "resume", title: "resume a thread", items: items, efforts: map[string]effortPick{}})
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
	kind, title := "tree", "choose the active branch"
	if op == "revertTree" {
		kind, title = "revert", "revert before a turn"
	}
	items := make([]listItem, 0, len(response.Turns))
	for _, turn := range response.Turns {
		if op == "revertTree" && !turn.Active {
			continue
		}
		items = append(items, listItem{id: sanitizeText(turn.TurnID), label: sanitizeText(turn.Preview), detail: sanitizeText(turn.TurnID), active: turn.Active})
	}
	if len(items) == 0 {
		m.noticef("no turns are available on this path; send a prompt first")
		return nil
	}
	m.openList(&listPanel{kind: kind, title: title, items: items, efforts: map[string]effortPick{}})
	return nil
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
