package tui

import (
	"encoding/json/jsontext"
	"fmt"
	"slices"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

type patchKey struct{ threadID, itemID string }

type approval struct {
	method    string
	requestID string
	threadID  string
	itemID    string
	command   string
	cwd       string
	reason    string
	scroll    int
	answer    chan requestAnswer
	card      *questionCard
}

func (m *model) applyRequest(msg serverRequestMsg) tea.Cmd {
	if msg.method == "item/tool/requestUserInput" {
		return m.applyQuestion(msg)
	}
	request, err := decodeApproval(msg.method, msg.params)
	if err != nil {
		msg.answer <- requestAnswer{err: err}
		m.errorf("approval request failed: %v; reconnect and retry the turn", err)
		return nil
	}
	request.requestID = msg.requestID
	request.answer = msg.answer
	m.input.field = textField{}
	m.approvals = append(m.approvals, request)
	if !m.acceptThread(request.threadID) {
		return nil
	}
	m.setState(stateAwaitingApproval)
	return m.notify(m.notifyLine("Waiting for approval"))
}

func (m *model) applyPatchUpdated(params jsontext.Value) error {
	event, err := decodeParams[rpc.FileChangePatchUpdatedNotification](params)
	if err != nil {
		return err
	}
	if m.live.patches == nil {
		m.live.patches = make(map[patchKey][]rpc.FileChange)
	}
	m.live.patches[patchKey{event.ThreadID, event.ItemID}] = sanitizeItem(rpc.Item{Changes: event.Changes}).Changes
	return nil
}

func (m *model) applyRequestResolved(params jsontext.Value) error {
	event, err := decodeParams[rpc.ServerRequestResolvedNotification](params)
	if err != nil {
		return err
	}
	index := slices.IndexFunc(m.approvals, func(open approval) bool { return open.requestID == event.RequestID })
	if index < 0 {
		return nil
	}
	card := m.approvals[index].card
	m.dropApproval(index)
	if card != nil {
		m.noticef("%s", questionResolvedNotice(event.Resolution))
	} else {
		m.noticef("%s", resolvedNotice(event.Resolution))
	}
	return nil
}

func resolvedNotice(resolution string) string {
	switch resolution {
	case "answered":
		return "Approval answered in another window"
	case "cancelled":
		return "Approval dismissed in another window"
	case "interrupted":
		return "Approval closed: the turn was interrupted"
	}
	return "Approval closed: the agent continues without an answer"
}

func (m *model) dropApproval(index int) {
	open := m.approvals[index]
	m.approvals = slices.Delete(m.approvals, index, index+1)
	delete(m.live.patches, patchKey{open.threadID, open.itemID})
	if len(m.approvals) == 0 && (m.thread.state == stateAwaitingApproval || m.thread.state == stateAwaitingInput) {
		m.setState(stateRunning)
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
	m.dropApproval(0)
	if key == "esc" {
		return m.interrupt()
	}
	return nil
}

func (m *model) approvalPanel() panelView {
	current := m.approvals[0]
	title := "Approval · " + approvalKind(current.method)
	if len(m.approvals) > 1 {
		title += fmt.Sprintf(" (1 of %d)", len(m.approvals))
	}
	title += " · " + m.screen.theme.paint(roleWarningText, "Waiting")
	return panelView{kind: "approval", id: current.itemID, layout: panelApproval, decision: true, title: title, body: m.approvalLines(current), actions: approvalChoices(&m.screen, threadHue(m.thread.id)), cursor: -1, scroll: current.scroll}
}

func approvalKind(method string) string {
	if method == "item/fileChange/requestApproval" {
		return "Apply file changes"
	}
	return "Run command"
}

func (m *model) approvalLines(current approval) []string {
	var lines []string
	if current.threadID != m.thread.id {
		lines = append(lines, "From thread "+current.threadID)
	}
	lines = append(lines, approvalDetails(current)...)
	for _, change := range m.approvalChanges(current) {
		lines = append(lines, m.approvalChangeLines(change)...)
	}
	return lines
}

func approvalDetails(current approval) []string {
	var lines []string
	if current.command != "" {
		cmdLines := strings.Split(sanitizeText(current.command), "\n")
		cmdLines[0] = "$ " + cmdLines[0]
		lines = append(lines, cmdLines...)
	}
	if current.cwd != "" {
		lines = append(lines, "cwd "+sanitizeText(current.cwd))
	}
	if current.reason != "" {
		lines = append(lines, "reason: "+sanitizeText(current.reason))
	}
	return lines
}

func (m *model) approvalChanges(current approval) []rpc.FileChange {
	if changes, ok := m.live.patches[patchKey{current.threadID, current.itemID}]; ok {
		return changes
	}
	index := slices.IndexFunc(m.live.items, func(item liveItem) bool { return item.ID == current.itemID })
	if index < 0 {
		return nil
	}
	return m.live.items[index].Changes
}

func (m *model) approvalChangeLines(change rpc.FileChange) []string {
	header := changeLabel(change.Kind) + " " + change.Path
	if change.DiffOmitted {
		return []string{header, "diff omitted, larger than 1 MiB"}
	}
	lines := []string{fmt.Sprintf("%s +%d −%d", header, change.Additions, change.Deletions)}
	hunk := false
	for _, line := range textLines(change.Diff) {
		hunk = hunk || strings.HasPrefix(line, "@@")
		if hunk {
			lines = append(lines, m.diffLine(line))
		}
	}
	return lines
}

func approvalChoices(screen *screenState, hue int) []string {
	return []string{screen.dockTheme().liveTag(hue, "[y] accept") + "  [a] accept for session", "[n] decline  [esc] cancel and interrupt"}
}
