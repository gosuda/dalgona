package tui

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

type approval struct {
	method   string
	threadID string
	itemID   string
	command  string
	reason   string
	answer   chan requestAnswer
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

func (m *model) renderApproval() string {
	current := m.approvals[0]
	lines := []string{"approval required"}
	if len(m.approvals) > 1 {
		lines[0] += fmt.Sprintf(" (1 of %d)", len(m.approvals))
	}
	if current.threadID != m.thread.id {
		lines = append(lines, "thread "+current.threadID)
	}
	if current.command != "" {
		lines = append(lines, "$ "+sanitizeText(current.command))
	}
	if current.reason != "" {
		lines = append(lines, "reason: "+sanitizeText(current.reason))
	}
	lines = append(lines, approvalChoices(m.screen.width)...)
	return wrapTerminal(strings.Join(lines, "\n"), m.screen.width)
}

func approvalChoices(width int) []string {
	choices := []string{"[y] accept", "[a] accept for session", "[n] decline", "[esc] cancel and interrupt"}
	if line := strings.Join(choices, "  "); lipgloss.Width(line) <= width {
		return []string{line}
	}
	return choices
}
