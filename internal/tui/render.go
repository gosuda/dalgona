package tui

import (
	json "encoding/json/v2"
	"strings"
	"unicode/utf8"

	"charm.land/glamour/v2"
	"charm.land/lipgloss/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

var faint = lipgloss.NewStyle().Faint(true)

type liveItem struct {
	rpc.Item
	text   []byte
	output []byte
}

func (l *liveItem) settle(final rpc.Item) rpc.Item {
	l.Text += string(l.text)
	l.Output += string(l.output)
	return mergeItem(final, l.Item)
}

func (l *liveItem) view() string {
	switch l.Type {
	case "commandExecution":
		return "$ " + l.Command + "\n" + l.Output + string(l.output)
	case "reasoning":
		return "thinking\n" + l.Text + string(l.text)
	default:
		return l.Text + string(l.text)
	}
}

func isAdvisorItem(item rpc.Item) bool {
	return item.Type == "dynamicToolCall" && item.Tool == "advisor"
}

func (m *model) renderAdvisorCard(advisor, severity, note string) string {
	advisor, severity, note = sanitizeText(advisor), sanitizeText(severity), sanitizeText(note)
	label := "advisor"
	if advisor != "" && advisor != label {
		label += " " + advisor
	}
	return faint.Render(wrapTerminal(label+" · "+severity+": "+note, m.width))
}

func sanitizeText(text string) string {
	var out strings.Builder
	out.Grow(len(text))
	for i := 0; i < len(text); {
		c := text[i]
		if c == 0x1b {
			i = skipEscapeSequence(text, i)
			continue
		}
		r, size := utf8.DecodeRuneInString(text[i:])
		if r < 0x20 && r != '\n' && r != '\t' || r == 0x7f || r >= 0x80 && r <= 0x9f {
			i += size
			continue
		}
		out.WriteString(text[i : i+size])
		i += size
	}
	return out.String()
}

func skipEscapeSequence(text string, i int) int {
	i++
	if i >= len(text) {
		return i
	}
	switch text[i] {
	case '[':
		return skipCSI(text, i)
	case ']', 'P', 'X', '^', '_':
		return skipStringSequence(text, i)
	default:
		return skipShortEscape(text, i)
	}
}

func skipCSI(text string, i int) int {
	for i++; i < len(text) && (text[i] < 0x40 || text[i] > 0x7e); i++ {
	}
	if i < len(text) {
		i++
	}
	return i
}

func skipStringSequence(text string, i int) int {
	for i++; i < len(text); i++ {
		switch {
		case text[i] == 0x07:
			return i + 1
		case text[i] == 0x1b && i+1 < len(text) && text[i+1] == '\\':
			return i + 2
		}
	}
	return i
}

func skipShortEscape(text string, i int) int {
	for i++; i < len(text) && text[i] >= 0x20 && text[i] <= 0x2f; i++ {
	}
	return i + 1
}

func sanitizeItem(item rpc.Item) rpc.Item {
	item.Command = sanitizeText(item.Command)
	item.Text = sanitizeText(item.Text)
	item.Output = sanitizeText(item.Output)
	item.Tool = sanitizeText(item.Tool)
	item.Server = sanitizeText(item.Server)
	item.Type = sanitizeText(item.Type)
	item.Status = sanitizeText(item.Status)
	return item
}

func (m *model) renderSettledItem(item rpc.Item) string {
	item = sanitizeItem(item)
	switch item.Type {
	case "userMessage":
		return wrapTerminal("you\n"+glowKeywords(item.Text, m.keywords, 0), m.width)
	case "agentMessage":
		return m.renderMarkdown(item.Text)
	case "reasoning":
		return wrapTerminal("thinking\n"+item.Text, m.width)
	case "commandExecution":
		return wrapTerminal(commandItemText(item), m.width)
	case "fileChange":
		return wrapTerminal(itemWithStatus("file change", item), m.width)
	case "mcpToolCall", "dynamicToolCall":
		return m.renderToolItem(item)
	case "contextCompaction":
		return wrapTerminal("context compacted\n"+item.Text, m.width)
	default:
		return wrapTerminal(itemWithStatus(item.Type, item), m.width)
	}
}

func (m *model) renderToolItem(item rpc.Item) string {
	var args struct {
		Advisor  string `json:"advisor"`
		Severity string `json:"severity"`
	}
	if isAdvisorItem(item) && json.Unmarshal(item.Arguments, &args) == nil {
		return m.renderAdvisorCard(args.Advisor, args.Severity, item.Output)
	}
	label := item.Tool
	if item.Type == "mcpToolCall" {
		label = "mcp " + item.Server + "/" + item.Tool
	}
	return wrapTerminal(itemWithStatus(label, item), m.width)
}

func statusLabel(status string) string {
	if status == "inProgress" {
		return "running"
	}
	return status
}

func commandItemText(item rpc.Item) string {
	text := "$ " + item.Command
	if item.Status != "" && item.Status != "completed" {
		text += " [" + statusLabel(item.Status) + "]"
	}
	if item.Output != "" {
		text += "\n" + item.Output
	}
	return text
}

func itemWithStatus(label string, item rpc.Item) string {
	if label == "" {
		label = "item"
	}
	text := label
	if item.Status != "" {
		text += " [" + statusLabel(item.Status) + "]"
	}
	if item.Text != "" {
		text += "\n" + item.Text
	}
	if item.Output != "" {
		text += "\n" + item.Output
	}
	return text
}

func (m *model) renderMarkdown(text string) string {
	width := terminalWidth(m.width)
	renderer, ok := m.markdown[width]
	if !ok {
		var err error
		renderer, err = glamour.NewTermRenderer(glamour.WithStandardStyle("dark"), glamour.WithWordWrap(width))
		if err != nil {
			return wrapTerminal("markdown unavailable: "+err.Error()+"\n"+text, width)
		}
		m.markdown[width] = renderer
	}
	rendered, err := renderer.Render(text)
	if err != nil {
		return wrapTerminal("markdown unavailable: "+err.Error()+"\n"+text, width)
	}
	return strings.TrimSpace(rendered)
}

func wrapTerminal(text string, width int) string {
	return lipgloss.Wrap(text, terminalWidth(width), "")
}

func terminalWidth(width int) int {
	if width < 1 {
		return 1
	}
	return width
}
