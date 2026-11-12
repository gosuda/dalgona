package tui

import (
	"cmp"
	"fmt"
	"slices"
	"strings"

	tea "charm.land/bubbletea/v2"
)

const composerMaxRows = 6

func (m *model) dockLines() []string {
	if m.screen.height < 2 {
		return nil
	}
	panel := m.panelLines()
	editorLimit := min(composerMaxRows, max(1, m.screen.height-3))
	if m.panelOpen() {
		editorLimit = 1
	}
	editor := m.editorRows(terminalWidth(m.screen.width), editorLimit)
	budget := m.screen.height - 1
	showBand, showLive := true, true
	count := func() int {
		n := len(editor) + len(panel)
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
	lines := slices.Clone(editor)
	lines = append(lines, panel...)
	if showLive {
		lines = append(lines, m.liveRow())
	}
	if showBand {
		lines = append(lines, m.statusRow())
	}
	return lines
}

func (m *model) panelOpen() bool {
	return len(m.approvals) > 0 || m.confirm != nil || m.selector != nil
}

func (m *model) editorRows(width, limit int) []string {
	c := m.input.composer
	text := c.draftView(m.draftSpans(), m.screen.theme) + c.placeholderView()
	rows := m.screen.wrap(text, width)
	if len(rows) <= limit {
		return rows
	}
	caret := len(m.screen.wrap("› "+string(c.text[:c.cursor]), width)) - 1
	start := min(max(0, caret-limit+1), len(rows)-limit)
	return rows[start : start+limit]
}

func (m *model) liveRow() string {
	parts := []string{"state: " + m.thread.state}
	if m.notice != "" {
		notice := strings.ReplaceAll(m.notice, "\n", " · ")
		if m.noticeError {
			notice = m.screen.dockTheme().paintBold(roleFailedText, notice)
		}
		parts = append(parts, notice)
	}
	if tail := m.liveTail(); tail != "" {
		parts = append(parts, tail)
	}
	return m.screen.fit(strings.Join(parts, " · "), terminalWidth(m.screen.width))
}

func (m *model) liveTail() string {
	for i := len(m.live.items) - 1; i >= 0; i-- {
		if isAdvisorItem(m.live.items[i].Item) {
			continue
		}
		if tail := m.live.items[i].tail(); tail != "" {
			return tail
		}
	}
	return ""
}

func (m *model) statusRow() string {
	sandbox := m.thread.sandbox
	if sandbox == "off" || sandbox == "" {
		sandbox = "no sandbox"
	}
	status := fmt.Sprintf("%s · %s · %s · %s", cmp.Or(m.thread.modelName, "default"), m.thread.approvalMode, sandbox, m.thread.title)
	if len(m.queue) > 0 {
		status += fmt.Sprintf(" · %d follow-up queued", len(m.queue))
	}
	return m.screen.fit(status, terminalWidth(m.screen.width))
}

func (m *model) panelLines() []string {
	switch {
	case len(m.approvals) > 0:
		return framePanel(&m.screen, m.approvalPanel(), panelRows(panelApproval, m.screen.height))
	case m.confirm != nil:
		return framePanel(&m.screen, m.confirmPanel(), panelRows(panelConfirm, m.screen.height))
	case m.selector != nil:
		return framePanel(&m.screen, m.selectorPanel(), panelRows(panelList, m.screen.height))
	default:
		return nil
	}
}

func (m model) dockView() tea.View {
	view := tea.NewView(strings.Join(m.dockLines(), "\n"))
	view.ReportFocus = true
	return view
}
