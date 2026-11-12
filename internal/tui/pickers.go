package tui

import (
	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

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

func (m *model) selectorPanel() panelView {
	body := make([]string, 0, len(m.selector.choices))
	for _, item := range m.selector.choices {
		active := ""
		if item.active {
			active = " (active)"
		}
		line := item.label + active
		if item.detail != "" {
			line += " — " + item.detail
		}
		body = append(body, line)
	}
	return panelView{
		kind:    m.selector.kind,
		layout:  panelList,
		title:   m.selector.title,
		body:    body,
		actions: []string{"↑/↓ choose · Enter select · Esc cancel"},
		cursor:  m.selector.cursor,
	}
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
		params := rpc.ThreadLeafSetParams{ThreadID: m.thread.id, TurnID: selected.id}
		return m.call("leaf", "dalgona/thread/leaf/set", params, nil, "")
	case "revert":
		params := rpc.ThreadRevertParams{ThreadID: m.thread.id, BeforeTurnID: selected.id}
		return m.call("revert", "thread/revert", params, nil, "")
	default:
		return nil
	}
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
		choices = append(choices, choice{id: sanitizeText(item.ID), label: sanitizeText(item.ID), detail: sanitizeText(item.DisplayName), active: item.ID == m.thread.modelName})
	}
	if len(choices) == 0 {
		m.noticef("no models are available; run /login <provider> and retry /model")
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
		choices = append(choices, choice{id: sanitizeText(item.ID), label: threadTitle(item), detail: threadState(item.Status), active: item.ID == m.thread.id})
	}
	if len(choices) == 0 {
		m.noticef("no threads are available; use /new to start one")
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
		choices = append(choices, choice{id: sanitizeText(turn.TurnID), label: sanitizeText(turn.Preview), detail: sanitizeText(turn.TurnID), active: turn.Active})
	}
	if len(choices) == 0 {
		m.noticef("no turns are available on this path; send a prompt first")
		return nil
	}
	m.selector = &selector{kind: kind, title: title, choices: choices}
	return nil
}
