package tui

import (
	"fmt"
	"strings"

	"charm.land/glamour/v2"
	"charm.land/glamour/v2/ansi"
	"charm.land/glamour/v2/styles"
)

type mdKey struct {
	width int
	theme theme
}

func paletteHex(t theme, r role) string {
	return fmt.Sprintf("#%06X", palettes[t.name][r])
}

func markdownStyle(t theme) ansi.StyleConfig {
	if !t.known || !t.color {
		return styles.NoTTYStyleConfig
	}
	cfg := styles.DarkStyleConfig
	if t.name == themeDay || t.name == themeSoftDay {
		cfg = styles.LightStyleConfig
	}
	text := paletteHex(t, roleText)
	accent := paletteHex(t, roleAccentText)
	bold := true
	for _, heading := range []*ansi.StyleBlock{&cfg.H1, &cfg.H2, &cfg.H3, &cfg.H4, &cfg.H5, &cfg.H6} {
		heading.Color = &text
		heading.BackgroundColor = nil
	}
	cfg.Document.Color = &text
	cfg.Heading.Color = &text
	cfg.Heading.Bold = &bold
	cfg.Link.Color = &accent
	cfg.LinkText.Color = &accent
	cfg.Code.Color = &text
	return cfg
}

func (m *model) renderMarkdown(text string) string {
	width := terminalWidth(m.screen.width)
	key := mdKey{width: width, theme: m.screen.theme}
	renderer, ok := m.out.markdown[key]
	if !ok {
		var err error
		renderer, err = glamour.NewTermRenderer(glamour.WithStyles(markdownStyle(m.screen.theme)), glamour.WithWordWrap(width))
		if err != nil {
			return "markdown unavailable: " + err.Error() + "\n" + text
		}
		m.out.markdown[key] = renderer
	}
	rendered, err := renderer.Render(text)
	if err != nil {
		return "markdown unavailable: " + err.Error() + "\n" + text
	}
	return strings.TrimSpace(rendered)
}

func (m *model) renderBlock(b block) []string {
	if b.code {
		return []string{b.text}
	}
	return append(textLines(m.renderMarkdown(b.text)), "")
}
