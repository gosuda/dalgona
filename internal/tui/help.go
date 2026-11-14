package tui

import (
	"fmt"
	"path/filepath"
	"slices"
	"strconv"
	"strings"

	tea "charm.land/bubbletea/v2"
)

const (
	helpHint   = "↑/↓ scroll · pgup/pgdown page · esc close"
	helpPage   = 12
	helpChrome = 2
)

type keyHelp struct {
	group, keys, action string
}

var keyTable = []keyHelp{
	{"Compose", "enter", "sends the prompt, runs a shell line, or activates the highlighted panel row"},
	{"Compose", "shift+enter, ctrl+j", "inserts a newline"},
	{"Compose", "\\ then enter", "replaces the backslash with a newline"},
	{"Compose", "alt+enter", "queues the draft as a follow-up"},
	{"Compose", "up, down", "move between wrapped rows; on the first or last line, recall history"},
	{"Compose", "alt+up", "moves the newest queued text back into an empty editor"},
	{"Compose", "alt+left, alt+right", "move by word"},
	{"Compose", "ctrl+w, ctrl+k, ctrl+u", "delete a word back, kill to line end, kill to line start"},
	{"Compose", "home, end", "move to the line start or end"},
	{"Compose", "tab", "completes the slash command or @file"},
	{"Compose", "backspace, delete", "remove a whole paste or shell chip"},
	{"Compose", "ctrl+v", "pastes an image from the clipboard"},
	{"Compose", "!", "on an empty prompt, starts shell mode; esc leaves it"},
	{"Compose", "/, //", "opens the slash palette; //text sends text that starts with /"},
	{"Compose", "@", "opens file completion"},
	{"Compose", "?", "on an empty prompt, opens this help panel"},
	{"Turn", "esc", "while working or waiting: the first press shows Esc again to interrupt; the second interrupts; one press in a rate-limit wait interrupts"},
	{"Turn", "esc", "on an empty idle prompt: the first press shows Esc again to rewind; the second opens the rewind list"},
	{"Turn", "ctrl+c", "clears a draft into history; otherwise the first press shows Ctrl+C again to exit and the second quits"},
	{"Turn", "ctrl+z", "suspends the client"},
	{"Turn", "ctrl+o", "opens the full-screen review"},
	{"Turn", "alt+q", "opens the queued follow-ups panel"},
	{"Panels", "up, ctrl+p, down, ctrl+n", "move the list cursor"},
	{"Panels", "pgup, pgdown", "move by a page"},
	{"Panels", "enter", "activates the highlighted row or confirms"},
	{"Panels", "esc", "clears the filter, then closes the list; cancels a confirm; closes the help panel"},
	{"Panels", "tab, shift+tab", "cycle the efforts of the highlighted model"},
	{"Panels", "ctrl+r", "opens the advisor reset confirm"},
	{"Panels", "y, a, n", "accept, accept for session, decline (approval card)"},
	{"Panels", "up, down", "scroll the approval card, settings rows and help lines; move the popup highlight"},
	{"Panels", "left, right, space", "settings: cycle a value, move between status items, toggle"},
	{"Review", "up, k, down, j", "scroll one line"},
	{"Review", "pgup, pgdown", "scroll a page"},
	{"Review", "home, g, end, G", "go to the top or bottom"},
	{"Review", "[, ]", "previous or next entry"},
	{"Review", "esc, q, ctrl+o", "close the review"},
}

type helpPanel struct {
	scroll int
}

func (m *model) openHelp() {
	m.help = &helpPanel{}
}

func (m *model) questionKey(msg tea.KeyPressMsg, key string) tea.Cmd {
	if m.input.mode == inputPrompt && m.input.composer.Value() == "" {
		m.openHelp()
		return nil
	}
	return m.composerKey(msg, key)
}

func (m *model) helpKey(key string) {
	switch key {
	case "up":
		m.help.scroll--
	case "down":
		m.help.scroll++
	case "pgup":
		m.help.scroll -= helpPage
	case "pgdown":
		m.help.scroll += helpPage
	case "esc", "?":
		m.help = nil
		return
	}
	m.clampHelp()
}

func (m *model) clampHelp() {
	rows := max(0, panelRows(panelHelp, m.screen.height)-helpChrome)
	limit := max(0, len(m.helpBody())-rows)
	m.help.scroll = min(max(m.help.scroll, 0), limit)
}

func (m *model) helpView() panelView {
	return panelView{kind: "help", layout: panelHelp, title: "Keys", body: m.helpBody(), actions: []string{helpHint}, cursor: -1, scroll: m.help.scroll}
}

func (m *model) helpBody() []string {
	body := make([]string, 0, len(keyTable)+helpPage)
	group := ""
	for _, row := range keyTable {
		if row.group != group {
			if group != "" {
				body = append(body, "")
			}
			body = append(body, row.group)
			group = row.group
		}
		body = append(body, fmt.Sprintf("  %-24s %s", row.keys, row.action))
	}
	body = append(body, "", "Status")
	return append(body, m.statusLegend()...)
}

func (m *model) statusLegend() []string {
	left, right := m.bandSegments()
	lines := make([]string, 0, len(left)+len(right))
	for _, seg := range slices.Concat(left, right) {
		lines = append(lines, m.legendLine(seg))
	}
	return lines
}

func (m *model) legendLine(seg segment) string {
	icon, value := m.legendIconValue(seg)
	return strings.Join(nonEmpty(icon, seg.id, value), " ")
}

func (m *model) legendIconValue(seg segment) (string, string) {
	icons := m.iconSet()
	switch seg.id {
	case "model", "mark":
		return "", seg.text
	case "effort":
		return seg.text, m.thread.effort
	case "approval":
		return seg.text, m.thread.approvalMode
	case "sandbox":
		return seg.text, m.thread.sandbox
	case "folder":
		return icons.folder, filepath.Base(m.conn.opts.Cwd)
	case "queue":
		return icons.queue, strconv.Itoa(len(m.queue))
	case "context":
		if icons.nerd {
			return legendNerdContext(seg.text)
		}
		return "", legendLastField(seg.text)
	}
	return "", ""
}

func legendNerdContext(text string) (string, string) {
	fields := strings.Fields(text)
	return fields[0], fields[len(fields)-1]
}

func legendLastField(text string) string {
	fields := strings.Fields(text)
	return fields[len(fields)-1]
}

func nonEmpty(parts ...string) []string {
	out := make([]string, 0, len(parts))
	for _, part := range parts {
		if part != "" {
			out = append(out, part)
		}
	}
	return out
}
