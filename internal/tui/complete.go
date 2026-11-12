package tui

import (
	"cmp"
	"slices"
	"strings"
	"unicode"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
)

const popupItems = 8

type completion struct {
	kind   string
	items  []listItem
	cursor int
}

func newCompletion(query string) *completion {
	return &completion{kind: "command", items: commandItems(query)}
}

func (c *completion) refilter(query string) {
	c.items = commandItems(query)
	c.cursor = min(c.cursor, max(0, len(c.items)-1))
}

func (c *completion) move(delta int) {
	c.cursor = min(max(c.cursor+delta, 0), max(0, len(c.items)-1))
}

func (c *completion) selected() string {
	return c.items[c.cursor].id
}

func commandItems(query string) []listItem {
	type ranked struct {
		item  listItem
		score int
	}
	var found []ranked
	for _, c := range commands {
		if score := fuzzyScore(query, c.name); score > 0 {
			found = append(found, ranked{item: listItem{id: c.name, label: commandLabel(c), detail: c.summary}, score: score})
		}
	}
	slices.SortStableFunc(found, func(a, b ranked) int { return cmp.Compare(b.score, a.score) })
	found = found[:min(len(found), popupItems)]
	items := make([]listItem, 0, len(found))
	for _, r := range found {
		items = append(items, r.item)
	}
	return items
}

func commandLabel(c command) string {
	if c.args == "" {
		return "/" + c.name
	}
	return "/" + c.name + " " + c.args
}

func commandNamed(name string) command {
	return commands[slices.IndexFunc(commands, func(c command) bool { return c.name == name })]
}

func splitToken(text string) (token, tail string) {
	end := strings.IndexFunc(text, unicode.IsSpace)
	if end < 0 {
		end = len(text)
	}
	return text[:end], text[end:]
}

func (m *model) commandQuery() (string, bool) {
	text := m.input.composer.Value()
	if m.panelOpen() || m.review != nil || m.textMode() != inputPrompt || !strings.HasPrefix(text, "/") || strings.HasPrefix(text, "//") {
		return "", false
	}
	token, _ := splitToken(text)
	if m.input.composer.cursor > utf8.RuneCountInString(token) {
		return "", false
	}
	return strings.TrimPrefix(token, "/"), true
}

func (m *model) syncPopup(typed bool) {
	query, ok := m.commandQuery()
	switch {
	case !ok:
		m.input.popup = nil
	case m.input.popup != nil:
		m.input.popup.refilter(query)
	case typed:
		m.input.popup = newCompletion(query)
	}
}

func (m *model) popupShown() bool {
	return m.input.popup != nil && !m.panelOpen() && m.review == nil
}

func (m *model) popupKey(msg tea.KeyPressMsg) (tea.Cmd, bool) {
	if !m.popupShown() {
		return nil, false
	}
	popup := m.input.popup
	switch msg.String() {
	case "esc":
		m.input.popup = nil
		return nil, true
	case "up", "down":
		if len(popup.items) == 0 {
			return nil, false
		}
		popup.move(map[string]int{"up": -1, "down": 1}[msg.String()])
		return nil, true
	case "tab":
		if len(popup.items) == 0 {
			return nil, false
		}
		m.completePopup()
		return nil, true
	case "enter":
		if len(popup.items) == 0 {
			return nil, false
		}
		return m.runPopup(), true
	}
	return nil, false
}

func (m *model) completePopup() {
	name := m.input.popup.selected()
	m.input.popup = nil
	_, tail := splitToken(m.input.composer.Value())
	tail = cmp.Or(tail, " ")
	head := "/" + name
	m.input.composer.SetValue(head + tail)
	m.input.composer.cursor = utf8.RuneCountInString(head + " ")
}

func (m *model) runPopup() tea.Cmd {
	name := m.input.popup.selected()
	_, tail := splitToken(m.input.composer.Value())
	if commandNamed(name).args != "" || tail != "" {
		m.completePopup()
		return nil
	}
	m.input.popup = nil
	m.input.composer.SetValue("/" + name)
	return m.submitPrompt()
}

func (m *model) popupLines() []string {
	popup := m.input.popup
	rows := panelRows(panelPopup, m.screen.height)
	width := terminalWidth(m.screen.width)
	lines := make([]string, 0, rows)
	for i := range rows {
		if i >= len(popup.items) {
			lines = append(lines, "")
			continue
		}
		item := popup.items[i]
		prefix := "  "
		if i == popup.cursor {
			prefix = "┃ "
		}
		line := m.screen.fit(prefix+item.label+"  "+item.detail, width)
		if i == popup.cursor {
			line = m.screen.dockTheme().paintBold(roleText, line)
		}
		lines = append(lines, line)
	}
	return lines
}

func typedText(msg tea.Msg) bool {
	switch msg := msg.(type) {
	case tea.KeyPressMsg:
		return msg.Text != ""
	case tea.PasteMsg:
		return msg.Content != ""
	}
	return false
}
