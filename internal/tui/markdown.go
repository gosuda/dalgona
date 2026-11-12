package tui

import (
	"strings"

	"charm.land/glamour/v2"
)

func (m *model) renderMarkdown(text string) string {
	width := terminalWidth(m.screen.width)
	renderer, ok := m.out.markdown[width]
	if !ok {
		var err error
		renderer, err = glamour.NewTermRenderer(glamour.WithStandardStyle("dark"), glamour.WithWordWrap(width))
		if err != nil {
			return "markdown unavailable: " + err.Error() + "\n" + text
		}
		m.out.markdown[width] = renderer
	}
	rendered, err := renderer.Render(text)
	if err != nil {
		return "markdown unavailable: " + err.Error() + "\n" + text
	}
	return strings.TrimSpace(rendered)
}
