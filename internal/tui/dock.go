package tui

import (
	"cmp"
	"fmt"
	"strings"
)

func (m *model) renderView() string {
	sections := make([]string, 0, 6)
	if live := m.renderLiveItems(); live != "" {
		sections = append(sections, live)
	}
	if len(m.approvals) > 0 {
		sections = append(sections, m.renderApproval())
	}
	if m.selector != nil {
		sections = append(sections, m.renderSelector())
	}
	if m.notice != "" {
		sections = append(sections, wrapTerminal(m.notice, m.screen.width))
	}
	sections = append(sections, "state: "+m.thread.state)
	composer := glowKeywords(m.input.composer.textView(), m.input.keywords, m.composerPhase()) + m.input.composer.placeholderView()
	sections = append(sections, wrapTerminal(composer, m.screen.width))
	sections = append(sections, m.statusRow())
	return strings.Join(sections, "\n")
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
	return wrapTerminal(status, m.screen.width)
}

func (m *model) renderLiveItems() string {
	lines := make([]string, 0, len(m.live.items))
	for _, item := range m.live.items {
		if isAdvisorItem(item.Item) {
			continue
		}
		lines = append(lines, wrapTerminal(item.view(), m.screen.width))
	}
	return strings.Join(lines, "\n")
}
