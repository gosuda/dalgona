package tui

import (
	"slices"
	"strconv"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

const composerMaxRows = 6

func (m *model) dock() ([]string, int, int) {
	if m.screen.height < 2 {
		return nil, 0, 0
	}
	panel := m.panelLines()
	editorLimit := min(composerMaxRows, max(1, m.screen.height-3))
	rows, row, col := m.inputRows(m.screen.width, editorLimit)
	budget := m.screen.height - 1
	showBand, showLive := true, true
	count := func() int {
		n := len(rows) + len(panel)
		if showLive {
			n++
		}
		if showBand {
			n++
		}
		return n
	}
	if count() > budget {
		showBand = false
	}
	if count() > budget {
		showLive = false
	}
	lines := slices.Clone(rows)
	lines = append(lines, panel...)
	if showLive {
		lines = append(lines, m.liveRow())
	}
	if showBand {
		lines = append(lines, m.bandRow())
	}
	return lines, row, col
}

func (m *model) dockLines() []string {
	lines, _, _ := m.dock()
	return lines
}

func (m *model) panelOpen() bool {
	return len(m.approvals) > 0 || m.confirm != nil || m.list != nil || m.settings != nil || m.login != nil
}

func (m *model) inputRows(width, limit int) ([]string, int, int) {
	if m.popupShown() {
		return m.editorRows(width, 1)
	}
	if !m.panelOpen() {
		return m.editorRows(width, limit)
	}
	line, col := m.panelInputLine(width)
	return []string{line}, 0, col
}

func (m *model) textMode() inputMode {
	if len(m.approvals) == 0 && m.confirm == nil && m.list != nil {
		if m.list.pending != "" {
			return inputTarget
		}
		return inputFilter
	}
	if m.login != nil && m.login.phase == loginAPIKey {
		return inputAPIKey
	}
	return m.input.mode
}

func (m *model) panelInputLine(width int) (string, int) {
	switch mode := m.textMode(); mode {
	case inputPrompt, inputShell:
		return m.draftLine(width)
	case inputFilter, inputTarget, inputAnswer, inputAPIKey:
		return m.fieldLine(mode, width)
	}
	return m.draftLine(width)
}

func (m *model) draftLine(width int) (string, int) {
	first := m.input.composer.firstLine()
	prompt := m.screen.dockTheme().hueText(threadHue(m.thread.id), promptText)
	line := m.screen.fit(prompt+first, terminalWidth(width))
	return line, min(promptCells+m.screen.cells(first), terminalWidth(width))
}

func (m *model) fieldLine(mode inputMode, width int) (string, int) {
	label := m.fieldLabel(mode)
	text := m.input.field.text
	if mode == inputAPIKey {
		text = []rune(strings.Repeat("*", len(text)))
	}
	room := terminalWidth(width) - m.screen.cells(label)
	window, caret := fieldWindow(&m.screen, text, m.input.field.cursor, room)
	return m.screen.fit(label+window, terminalWidth(width)), m.screen.cells(label) + caret
}

func (m *model) fieldLabel(mode inputMode) string {
	if mode == inputTarget {
		return reviewLabel(m.list.pending) + ": "
	}
	return inputLabel(mode)
}

func fieldWindow(s *screenState, text []rune, cursor, room int) (string, int) {
	if s.cells(string(text)) <= room {
		return string(text), s.cells(string(text[:cursor]))
	}
	left, right := growWindow(s, text, cursor, room)
	prefix, suffix := "", ""
	if left > 0 {
		prefix = "..."
	}
	if right < len(text) {
		suffix = "..."
	}
	return prefix + string(text[left:right]) + suffix, len(prefix) + s.cells(string(text[left:cursor]))
}

func growWindow(s *screenState, text []rune, cursor, room int) (int, int) {
	left, right := cursor, cursor
	for grown := true; grown; {
		grown = false
		if right < len(text) && windowCells(s, text, left, right+1) <= room {
			right++
			grown = true
		}
		if left > 0 && windowCells(s, text, left-1, right) <= room {
			left--
			grown = true
		}
	}
	return left, right
}

func windowCells(s *screenState, text []rune, left, right int) int {
	n := s.cells(string(text[left:right]))
	if left > 0 {
		n += 3
	}
	if right < len(text) {
		n += 3
	}
	return n
}

func (m *model) editorRows(width, limit int) ([]string, int, int) {
	lines, row, col := m.input.composer.layout(&m.screen, width, m.draftSpans(), m.screen.dockTheme())
	start := max(0, row-limit+1)
	end := min(len(lines), start+limit)
	prompt := m.screen.dockTheme().hueText(threadHue(m.thread.id), promptText)
	rows := make([]string, 0, end-start)
	for i := start; i < end; i++ {
		lead := strings.Repeat(" ", promptCells)
		if i == 0 {
			lead = prompt
		}
		rows = append(rows, lead+lines[i])
	}
	return rows, row - start, col + promptCells
}

const (
	liveTagCells   = 10
	liveStateCells = 8
	elapsedCells   = 7
	liveCallsCells = 7
	livePlanCells  = 12
	liveQueueCells = 10
)

type liveField struct {
	text  string
	cells int
}

func stateWord(state string, lastTurn time.Duration) string {
	switch state {
	case stateRunning:
		return "Working"
	case stateAwaitingApproval, "awaiting input":
		return "Waiting"
	case stateInterrupted:
		return "Stopped"
	case stateFailed:
		return "Failed"
	case stateDisconnected:
		return "Offline"
	case stateIdle:
		if lastTurn > 0 {
			return "Done"
		}
		return "Ready"
	}
	return ""
}

func (m *model) liveRow() string {
	width := terminalWidth(m.screen.width)
	var row strings.Builder
	for _, field := range m.liveFields(width) {
		row.WriteString(field.text)
	}
	row.WriteString(m.liveText())
	return m.screen.fit(row.String(), width)
}

func (m *model) liveFields(width int) []liveField {
	fields := []liveField{
		{text: m.liveTag(), cells: liveTagCells},
		{text: m.liveElapsed(), cells: elapsedCells},
		{text: m.liveCalls(), cells: liveCallsCells},
		{text: m.livePlan(), cells: livePlanCells},
		{text: m.liveQueued(), cells: liveQueueCells},
	}
	for _, index := range []int{4, 3, 2} {
		if fieldsWidth(fields) <= width {
			break
		}
		fields = slices.Delete(fields, index, index+1)
	}
	return fields
}

func fieldsWidth(fields []liveField) int {
	total := 0
	for _, field := range fields {
		total += field.cells
	}
	return total
}

func (m *model) liveFreeWidth() int {
	width := terminalWidth(m.screen.width)
	return width - fieldsWidth(m.liveFields(width))
}

func (s *screenState) pad(text string, n int) string {
	return text + strings.Repeat(" ", max(0, n-s.cells(text)))
}

func (s *screenState) padLeft(text string, n int) string {
	return strings.Repeat(" ", max(0, n-s.cells(text))) + text
}

func (m *model) liveTag() string {
	word := stateWord(m.thread.state, m.thread.lastTurn)
	if m.thread.state == stateIdle || word == "" {
		return strings.Repeat(" ", liveTagCells)
	}
	theme := m.screen.dockTheme()
	padded := m.screen.pad(m.screen.fit(word, liveStateCells), liveStateCells)
	if m.thread.state == stateRunning && len(m.approvals) == 0 {
		return theme.liveTag(threadHue(m.thread.id), m.screen.spin.View()+" "+padded)
	}
	switch m.thread.state {
	case stateFailed:
		return "  " + theme.paintBold(roleFailedText, padded)
	case stateDisconnected:
		return "  " + theme.paint(roleWarningText, padded)
	default:
		return "  " + theme.paint(roleDim, padded)
	}
}

func (m *model) liveElapsed() string {
	if !m.turnLive() || m.thread.turnStartedAt.IsZero() {
		return strings.Repeat(" ", elapsedCells)
	}
	text := formatElapsed(m.now().Sub(m.thread.turnStartedAt))
	return m.screen.padLeft(m.screen.dockTheme().paint(roleDim, text), elapsedCells)
}

func (m *model) liveCalls() string {
	count := 0
	for _, item := range m.live.items {
		if isToolItem(item.Item) {
			count++
		}
	}
	if count == 0 {
		return strings.Repeat(" ", liveCallsCells)
	}
	return m.screen.pad(m.screen.dockTheme().paint(roleDim, countText(count, "run")), liveCallsCells)
}

func (m *model) livePlan() string {
	if len(m.live.plan) == 0 {
		return strings.Repeat(" ", livePlanCells)
	}
	done := 0
	for _, step := range m.live.plan {
		if step.Status == "completed" {
			done++
		}
	}
	text := numberText(done) + "/" + numberText(len(m.live.plan)) + " plan"
	return m.screen.pad(m.screen.dockTheme().paint(roleDim, text), livePlanCells)
}

func (m *model) liveQueued() string {
	if len(m.queue) == 0 {
		return strings.Repeat(" ", liveQueueCells)
	}
	return m.screen.pad(m.screen.dockTheme().paint(roleDim, countText(len(m.queue), "queued")), liveQueueCells)
}

func countText(n int, word string) string {
	return numberText(n) + " " + word
}

func numberText(n int) string {
	if n > 99 {
		return "99+"
	}
	return strconv.Itoa(n)
}

func isToolItem(item rpc.Item) bool {
	switch item.Type {
	case "commandExecution", "fileChange", "mcpToolCall", "dynamicToolCall":
		return !isAdvisorItem(item)
	}
	return false
}

func (m *model) liveText() string {
	switch {
	case m.hint.text != "":
		return m.screen.dockTheme().paint(roleDim, m.hint.text)
	case m.settings != nil && m.settings.saveErr != "":
		return m.screen.dockTheme().paintBold(roleFailedText, m.settings.saveErr)
	case m.notice != "":
		notice := m.noticeLine()
		if m.noticeError {
			return m.screen.dockTheme().paintBold(roleFailedText, notice)
		}
		return notice
	}
	return m.activity()
}

func (m *model) noticeLine() string {
	return strings.ReplaceAll(m.notice, "\n", " · ")
}

func (m *model) activity() string {
	if len(m.approvals) > 0 || m.thread.state == stateAwaitingApproval {
		return "Waiting for your approval"
	}
	if left, ok := m.retryRemaining(); ok {
		return retryLine(left)
	}
	for i := len(m.live.items) - 1; i >= 0; i-- {
		if !isAdvisorItem(m.live.items[i].Item) {
			return m.live.items[i].activity()
		}
	}
	return ""
}

func activityLabel(item rpc.Item) string {
	switch item.Type {
	case "reasoning":
		return "Thinking"
	case "agentMessage":
		return "Writing"
	case "commandExecution":
		return "Running " + strings.SplitN(item.Command, "\n", 2)[0]
	case "fileChange":
		if len(item.Changes) == 0 {
			return "Editing files"
		}
		return "Editing " + item.Changes[0].Path
	case "mcpToolCall":
		return "Calling " + item.Server + "/" + item.Tool
	case "dynamicToolCall":
		if item.Tool == "task" {
			return "Subagent"
		}
		return item.Tool
	}
	return ""
}

func (m *model) panelLines() []string {
	switch {
	case len(m.approvals) > 0:
		return framePanel(&m.screen, m.approvalPanel(), panelRows(panelApproval, m.screen.height))
	case m.confirm != nil:
		return framePanel(&m.screen, m.confirmPanel(), panelRows(panelConfirm, m.screen.height))
	case m.list != nil:
		return framePanel(&m.screen, m.listPanelView(), panelRows(panelList, m.screen.height))
	case m.popupShown():
		return m.popupLines()
	case m.settings != nil:
		return framePanel(&m.screen, m.settingsView(), panelRows(panelSettings, m.screen.height))
	case m.login != nil:
		return framePanel(&m.screen, m.loginPanelView(), panelRows(panelLogin, m.screen.height))
	default:
		return nil
	}
}

func (m model) dockView() tea.View {
	lines, row, col := m.dock()
	view := tea.NewView(strings.Join(lines, "\n"))
	view.ReportFocus = true
	view.Cursor = tea.NewCursor(col, row)
	view.Cursor.Blink = m.screen.motion
	return view
}
