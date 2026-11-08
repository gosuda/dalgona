package tui

import (
	json "encoding/json/v2"
	"strings"

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
	label := "advisor"
	if advisor != "" && advisor != label {
		label += " " + advisor
	}
	return faint.Render(wrapTerminal(label+" · "+severity+": "+note, m.width))
}

func (m *model) renderSettledItem(item rpc.Item) string {
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
