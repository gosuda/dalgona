package tui

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

type approval struct {
	method   string
	threadID string
	itemID   string
	command  string
	reason   string
	scroll   int
	answer   chan requestAnswer
}

func (m *model) applyRequest(msg serverRequestMsg) {
	request, err := decodeApproval(msg.method, msg.params)
	if err != nil {
		msg.answer <- requestAnswer{err: err}
		m.errorf("approval request failed: %v; reconnect and retry the turn", err)
		return
	}
	request.answer = msg.answer
	m.approvals = append(m.approvals, request)
	if m.acceptThread(request.threadID) {
		m.setState(stateAwaitingApproval)
	}
}

var approvalDecisions = map[string]string{
	"y":   "accept",
	"a":   "acceptForSession",
	"n":   "decline",
	"esc": "cancel",
}

func (m *model) scrollApproval(key string) bool {
	switch key {
	case "up":
		m.approvals[0].scroll = max(m.approvals[0].scroll-1, 0)
	case "down":
		m.approvals[0].scroll++
	default:
		return false
	}
	return true
}

func (m *model) answerApprovalKey(key string) tea.Cmd {
	if m.scrollApproval(key) {
		return nil
	}
	decision, ok := approvalDecisions[key]
	if !ok {
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

func (m *model) approvalPanel() panelView {
	current := m.approvals[0]
	title := "approval required"
	if len(m.approvals) > 1 {
		title += fmt.Sprintf(" (1 of %d)", len(m.approvals))
	}
	var body []string
	if current.threadID != m.thread.id {
		body = append(body, "thread "+current.threadID)
	}
	if current.command != "" {
		for i, line := range strings.Split(sanitizeText(current.command), "\n") {
			if i == 0 {
				line = "$ " + line
			}
			body = append(body, line)
		}
	}
	if current.reason != "" {
		body = append(body, "reason: "+sanitizeText(current.reason))
	}
	return panelView{kind: "approval", id: current.itemID, layout: panelApproval, decision: true, title: title, body: body, actions: approvalChoices(&m.screen, terminalWidth(m.screen.width)), cursor: -1, scroll: current.scroll}
}

func approvalChoices(screen *screenState, width int) []string {
	choices := []string{"[y] accept", "[a] accept for session", "[n] decline", "[esc] cancel and interrupt"}
	if line := strings.Join(choices, "  "); screen.cells(line) <= width {
		return []string{line}
	}
	return choices
}
