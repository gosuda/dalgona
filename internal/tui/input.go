package tui

import tea "charm.land/bubbletea/v2"

type composer struct {
	text        []rune
	cursor      int
	width       int
	focused     bool
	placeholder string
}

func newComposer() composer {
	return composer{width: 80, focused: true, placeholder: "Send a prompt; / for commands"}
}

func (c composer) Value() string {
	return string(c.text)
}

func (c *composer) SetValue(value string) {
	c.text = []rune(value)
	c.cursor = len(c.text)
}

func (c *composer) SetWidth(width int) {
	c.width = width
}

func (c *composer) Focus() tea.Cmd {
	c.focused = true
	return nil
}

func (c composer) draftView(spans []keywordSpan, t theme) string {
	prefix := "› "
	cut := len(prefix) + len(string(c.text[:c.cursor]))
	text, marked := insertCursor(prefix+c.Value(), shiftSpans(spans, len(prefix)), cut, c.cursorView())
	return markKeywords(text, marked, t)
}

func (c composer) placeholderView() string {
	if len(c.text) > 0 {
		return ""
	}
	return faint.Render(c.placeholder)
}

func (c composer) cursorView() string {
	if !c.focused {
		return ""
	}
	return "▏"
}

func (c composer) Update(msg tea.Msg) (composer, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.PasteMsg:
		c.insert([]rune(sanitizeText(msg.Content)))
	case tea.KeyPressMsg:
		c.updateKey(msg)
	}
	return c, nil
}

func (c *composer) updateKey(msg tea.KeyPressMsg) {
	key := msg.String()
	switch key {
	case "backspace":
		c.backspace()
	case "delete":
		c.delete()
	case "ctrl+u":
		c.text = c.text[c.cursor:]
		c.cursor = 0
	case "left", "right", "home", "ctrl+a", "end", "ctrl+e":
		c.navigate(key)
	default:
		if msg.Text != "" {
			c.insert([]rune(sanitizeText(msg.Text)))
		}
	}
}

func (c *composer) navigate(key string) {
	switch key {
	case "left":
		c.moveLeft()
	case "right":
		c.moveRight()
	case "home", "ctrl+a":
		c.cursor = 0
	case "end", "ctrl+e":
		c.cursor = len(c.text)
	}
}

func (c *composer) insert(text []rune) {
	if len(text) == 0 {
		return
	}
	updated := make([]rune, 0, len(c.text)+len(text))
	updated = append(updated, c.text[:c.cursor]...)
	updated = append(updated, text...)
	updated = append(updated, c.text[c.cursor:]...)
	c.text = updated
	c.cursor += len(text)
}

func (c *composer) backspace() {
	if c.cursor == 0 {
		return
	}
	c.text = append(c.text[:c.cursor-1], c.text[c.cursor:]...)
	c.cursor--
}

func (c *composer) delete() {
	if c.cursor >= len(c.text) {
		return
	}
	c.text = append(c.text[:c.cursor], c.text[c.cursor+1:]...)
}

func (c *composer) moveLeft() {
	if c.cursor > 0 {
		c.cursor--
	}
}

func (c *composer) moveRight() {
	if c.cursor < len(c.text) {
		c.cursor++
	}
}
