package tui

import (
	"fmt"
	"slices"
)

type panelKind int

const (
	panelPopup panelKind = iota
	panelList
	panelConfirm
	panelApproval
	panelAsk
	panelSettings
	panelLogin
	panelHelp
)

var panelHeights = [...]int{8, 12, 6, 9, 13, 17, 12, 14}

func panelRows(kind panelKind, windowHeight int) int {
	return max(0, min(panelHeights[kind], windowHeight-4))
}

type panelView struct {
	kind     string
	id       string
	layout   panelKind
	decision bool
	title    string
	body     []string
	actions  []string
	cursor   int
	scroll   int
}

func framePanel(s *screenState, p panelView, rows int) []string {
	if rows <= 0 {
		return nil
	}
	width := terminalWidth(s.width)
	actions := p.actions
	if len(actions) > rows-1 {
		actions = actions[len(actions)-(rows-1):]
	}
	bodyRows := rows - 1 - len(actions)
	lines := []string{s.fit("┌ "+p.title, width)}
	start, shown := bodyWindow(p, bodyRows)
	for i, line := range shown {
		prefix := "│ "
		if start+i == p.cursor {
			prefix = "┃ "
		}
		lines = append(lines, s.fit(prefix+line, width))
	}
	for len(lines) < rows-len(actions) {
		lines = append(lines, s.fit("│", width))
	}
	for i, action := range actions {
		prefix := "   "
		if i == 0 {
			prefix = "└─ "
		}
		lines = append(lines, s.fit(prefix+action, width))
	}
	return lines
}

func bodyWindow(p panelView, rows int) (int, []string) {
	if rows <= 0 {
		return 0, nil
	}
	start, end := bodySpan(p, rows)
	if end == len(p.body) {
		return start, slices.Clone(p.body[start:end])
	}
	start, end = bodySpan(p, rows-1)
	shown := slices.Clone(p.body[start:end])
	if end < len(p.body) {
		shown = append(shown, fmt.Sprintf("↓ %d more lines · ↑↓ scroll", len(p.body)-end))
	}
	return start, shown
}

func bodySpan(p panelView, rows int) (int, int) {
	start := min(max(p.scroll, 0), max(len(p.body)-rows, 0))
	if p.cursor >= 0 {
		start = min(start, p.cursor)
		if p.cursor >= start+rows {
			start = p.cursor - rows + 1
		}
	}
	start = min(start, max(len(p.body)-1, 0))
	return start, min(start+rows, len(p.body))
}
