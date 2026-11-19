package tui

import (
	"cmp"
	"fmt"
	"slices"
	"strings"
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

const panelBodyCells = 56

const panelRailCells = 2

const panelRowCells = panelRailCells + panelBodyCells

var panelHeights = [...]int{8, 12, 6, 9, 13, 17, 12, 14}

const (
	decisionMinWindow = 4
	decisionMinRows   = 3
)

func decisionPanel(kind panelKind) bool {
	return kind == panelApproval || kind == panelAsk
}

func panelRows(kind panelKind, windowHeight int) int {
	rows := min(panelHeights[kind], windowHeight-4)
	if decisionPanel(kind) && windowHeight >= decisionMinWindow {
		rows = max(rows, min(decisionMinRows, windowHeight-2))
	}
	return max(0, rows)
}

func panelRowWidth(s *screenState) int {
	return min(terminalWidth(s.width), panelRowCells)
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

type listItem struct {
	id     string
	label  string
	date   string
	detail string
	active bool
}

type effortPick struct {
	levels  []string
	base    string
	current string
}

type listPanel struct {
	kind          string
	title         string
	items         []listItem
	cursor        int
	scroll        int
	err           string
	efforts       map[string]effortPick
	pendingEffort string
	pending       string
	empty         string
	revision      uint64
}

func (p *listPanel) visible(query string) []int {
	type ranked struct{ index, score int }
	ranks := make([]ranked, 0, len(p.items))
	for i, item := range p.items {
		if score := fuzzyScore(query, item.label); score > 0 {
			ranks = append(ranks, ranked{index: i, score: score})
		}
	}
	slices.SortStableFunc(ranks, func(a, b ranked) int { return cmp.Compare(b.score, a.score) })
	indexes := make([]int, len(ranks))
	for i, r := range ranks {
		indexes[i] = r.index
	}
	return indexes
}

func fuzzyScore(query, text string) int {
	q := strings.ToLower(query)
	t := strings.ToLower(text)
	switch {
	case strings.HasPrefix(t, q):
		return 3
	case strings.Contains(t, q):
		return 2
	case isSubsequence(q, t):
		return 1
	default:
		return 0
	}
}

func isSubsequence(query, text string) bool {
	rest := []rune(query)
	for _, r := range text {
		if len(rest) > 0 && r == rest[0] {
			rest = rest[1:]
		}
	}
	return len(rest) == 0
}

func panelBodyRows(p panelView, rows int) int {
	return max(rows-1-min(len(p.actions), max(rows-1, 0)), 0)
}

func framePanel(s *screenState, p panelView, rows int) []string {
	if rows <= 0 {
		return nil
	}
	rowTheme := s.dockTheme()
	width := panelRowWidth(s)
	actions := p.actions
	if len(actions) > rows-1 {
		actions = actions[len(actions)-(rows-1):]
	}
	bodyRows := panelBodyRows(p, rows)
	head := rowTheme.paint(roleFaint, "┌ ") + rowTheme.paintBold(roleText, s.fit(p.title, width-panelRailCells))
	lines := []string{s.fit(head, width)}
	start, shown := bodyWindow(p, bodyRows)
	marked := p.cursor >= 0
	for i, line := range shown {
		global := start + i
		prefix := rowTheme.paint(roleFaint, "│ ")
		marker := "  "
		if marked && global == p.cursor {
			prefix = rowTheme.paintBold(roleText, "┃ ")
			marker = rowTheme.paintBold(roleAccentText, "● ")
		}
		lines = append(lines, s.fit(prefix+marker+line, width))
	}
	for len(lines) < rows-len(actions) {
		lines = append(lines, s.fit(rowTheme.paint(roleFaint, "│"), width))
	}
	actionWidth := terminalWidth(s.width)
	for i, action := range actions {
		prefix := "   "
		if i == 0 {
			prefix = "└─ "
		}
		line := rowTheme.paint(roleFaint, prefix) + rowTheme.paint(roleDim, s.fit(action, actionWidth-s.cells(prefix)))
		lines = append(lines, s.fit(line, actionWidth))
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
		shown = append(shown, fmt.Sprintf("↓ %d more lines · ↑↓ scroll · ctrl+o full view", len(p.body)-end))
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
