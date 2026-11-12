package tui

import (
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

const revertNotice = "Later turns leave the active path. Files changed by later turns are not reverted."

type confirmation struct {
	turnID  string
	preview string
}

func (m *model) openRevert(turnID, preview string) {
	m.confirm = &confirmation{turnID: turnID, preview: preview}
	m.input.field = textField{}
}

func (m *model) confirmPanel() panelView {
	width := terminalWidth(m.screen.width)
	flat := strings.Join(strings.Fields(m.confirm.preview), " ")
	body := []string{"`" + m.screen.fit(flat, width-2) + "`"}
	rest := m.screen.wrap(revertNotice, width)
	if len(rest) > 3 {
		rest = append(rest[:2], m.screen.fit(strings.Join(rest[2:], " "), width))
	}
	return panelView{
		kind:     "revert",
		id:       m.confirm.turnID,
		layout:   panelConfirm,
		decision: true,
		title:    "Revert to before this turn?",
		body:     append(body, rest...),
		actions:  []string{"[enter] revert  [esc] keep"},
		cursor:   -1,
	}
}

func (m *model) confirmKey(key string) tea.Cmd {
	switch key {
	case "enter":
		params := rpc.ThreadRevertParams{ThreadID: m.thread.id, BeforeTurnID: m.confirm.turnID}
		m.confirm = nil
		return m.call("revert", "thread/revert", params, nil, "")
	case "esc":
		m.confirm = nil
	}
	return nil
}
