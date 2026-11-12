package tui

import (
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

const (
	revertNotice = "Later turns leave the active path. Files changed by later turns are not reverted."
	rewindNotice = "The prompt returns to the editor. Files changed by later turns are not reverted."
)

const (
	confirmRevert       = "revert"
	confirmRewind       = "rewind"
	confirmNew          = "new"
	confirmCompact      = "compact"
	confirmAdvisorReset = "advisorReset"
)

type confirmation struct {
	kind    string
	id      string
	title   string
	preview string
	lines   []string
	action  string
}

func (m *model) openConfirm(c confirmation) {
	m.confirm = &c
	m.input.field = textField{}
}

func (m *model) openRevert(turnID, preview string) {
	m.openConfirm(confirmation{kind: confirmRevert, id: turnID, title: "Revert to before this turn?", preview: preview, action: "[enter] revert  [esc] keep"})
}

func (m *model) openRewind(turnID, preview string) {
	m.openConfirm(confirmation{kind: confirmRewind, id: turnID, title: "Rewind to before this turn?", preview: preview, action: "[enter] rewind  [esc] keep"})
}

func (m *model) confirmPanel() panelView {
	c := m.confirm
	return panelView{
		kind:     c.kind,
		id:       c.id,
		layout:   panelConfirm,
		decision: true,
		title:    c.title,
		body:     m.confirmBody(c),
		actions:  []string{c.action},
		cursor:   -1,
	}
}

func (m *model) confirmBody(c *confirmation) []string {
	width := terminalWidth(m.screen.width)
	if c.kind != confirmRevert && c.kind != confirmRewind {
		body := make([]string, 0, len(c.lines))
		for _, line := range c.lines {
			body = append(body, m.screen.fit(line, width))
		}
		return body
	}
	var body []string
	notice := rewindNotice
	if c.kind == confirmRevert {
		flat := strings.Join(strings.Fields(c.preview), " ")
		body = append(body, "`"+m.screen.fit(flat, width-2)+"`")
		notice = revertNotice
	}
	rest := m.screen.wrap(notice, width)
	if len(rest) > 3 {
		rest = append(rest[:2], m.screen.fit(strings.Join(rest[2:], " "), width))
	}
	return append(body, rest...)
}

func (m *model) confirmKey(key string) tea.Cmd {
	switch key {
	case "enter":
		c := *m.confirm
		m.confirm = nil
		return m.confirmed(c)
	case "esc":
		m.confirm = nil
	}
	return nil
}

func (m *model) confirmed(c confirmation) tea.Cmd {
	switch c.kind {
	case confirmRevert:
		params := rpc.ThreadRevertParams{ThreadID: m.thread.id, BeforeTurnID: c.id}
		return m.call("revert", "thread/revert", params, nil, "")
	case confirmRewind:
		params := rpc.ThreadRevertParams{ThreadID: m.thread.id, BeforeTurnID: c.id}
		return m.call("rewind", "thread/revert", params, nil, c.preview)
	case confirmNew:
		return m.newThread()
	case confirmCompact:
		params := rpc.ThreadCompactStartParams{ThreadID: m.thread.id}
		return m.call("compact", "thread/compact/start", params, nil, "")
	case confirmAdvisorReset:
		params := rpc.DalgonaAdvisorStatusParams{ThreadID: m.thread.id}
		return m.call("advisorReset", "dalgona/advisor/reset", params, nil, "")
	}
	return nil
}

func contextFraction(usage rpc.ThreadTokenUsage) (float64, bool) {
	if usage.ModelContextWindow <= 0 {
		return 0, false
	}
	last := usage.Last
	used := last.InputTokens + last.CachedInputTokens + last.CacheWriteInputTokens + last.OutputTokens
	return min(1, max(0, float64(used)/float64(usage.ModelContextWindow))), true
}
