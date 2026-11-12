package tui

import (
	json "encoding/json/v2"
	"slices"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

var faint = lipgloss.NewStyle().Faint(true)

const (
	settleDelay     = 3 * time.Second / framesPerSecond
	maxSettledItems = 200
)

type printedMsg struct{}

type settledMsg struct {
	n int
}

type frameGeometry struct {
	rows      int
	width     int
	height    int
	altScreen bool
}

type entry struct {
	key  string
	text string
}

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

func (l *liveItem) tail() string {
	if l.Type == "commandExecution" {
		if last := lastLine(l.Output + string(l.output)); last != "" {
			return "$ " + l.Command + " · " + last
		}
		return "$ " + l.Command
	}
	return lastLine(l.Text + string(l.text))
}

func lastLine(text string) string {
	lines := strings.Split(strings.TrimRight(text, "\n"), "\n")
	return strings.TrimSpace(lines[len(lines)-1])
}

func isAdvisorItem(item rpc.Item) bool {
	return item.Type == "dynamicToolCall" && item.Tool == "advisor"
}

func (m *model) commit(key string, lines []string) {
	if _, ok := m.out.printed[key]; ok {
		return
	}
	m.out.printed[key] = struct{}{}
	m.out.entries = append(m.out.entries, entry{key: key, text: strings.Join(lines, "\n")})
	m.out.pending = append(m.out.pending, lines...)
}

func (m *model) settle() tea.Cmd {
	geometry := m.currentGeometry()
	if geometry == m.out.geometry {
		return nil
	}
	m.out.geometry = geometry
	m.out.settling = true
	m.out.settleN++
	n := m.out.settleN
	return tea.Tick(settleDelay, func(time.Time) tea.Msg { return settledMsg{n: n} })
}

func (m *model) currentGeometry() frameGeometry {
	view := m.View()
	return frameGeometry{
		rows:      lineCount(view.Content),
		width:     m.screen.width,
		height:    m.screen.height,
		altScreen: view.AltScreen,
	}
}

func lineCount(content string) int {
	if content == "" {
		return 0
	}
	return strings.Count(content, "\n") + 1
}

func (m *model) drain() tea.Cmd {
	if m.out.printing || m.out.settling || len(m.out.pending) == 0 {
		return nil
	}
	bound := m.screen.height - len(m.dockLines())
	if bound < 1 {
		return nil
	}
	rows := m.takeChunk(bound)
	if len(rows) == 0 {
		return nil
	}
	m.out.frozen = m.View()
	m.out.printing = true
	printed := func() tea.Msg { return printedMsg{} }
	return tea.Sequence(tea.Println(chunkText(rows)), printed)
}

func (m *model) takeChunk(bound int) []string {
	var rows []string
	width := terminalWidth(m.screen.width)
	for len(m.out.pending) > 0 && len(rows) < bound {
		wrapped := m.screen.wrap(m.out.pending[0], width)
		m.out.pending = m.out.pending[1:]
		room := bound - len(rows)
		if len(wrapped) > room {
			m.out.pending = append(slices.Clone(wrapped[room:]), m.out.pending...)
			wrapped = wrapped[:room]
		}
		rows = append(rows, wrapped...)
	}
	return rows
}

func chunkText(rows []string) string {
	text := strings.Join(rows, "\n")
	if text == "" {
		return " "
	}
	return text
}

func (m *model) renderAdvisorCard(advisor, severity, note string) string {
	advisor, severity, note = sanitizeText(advisor), sanitizeText(severity), sanitizeText(note)
	label := "advisor"
	if advisor != "" && advisor != label {
		label += " " + advisor
	}
	return faint.Render(label + " · " + severity + ": " + note)
}

func (m *model) renderSettledItem(item rpc.Item) string {
	item = sanitizeItem(item)
	switch item.Type {
	case "userMessage":
		return "you\n" + glowKeywords(item.Text, m.input.keywords, 0)
	case "agentMessage":
		return m.renderMarkdown(item.Text)
	case "reasoning":
		return "thinking\n" + item.Text
	case "commandExecution":
		return commandItemText(item)
	case "fileChange":
		return itemWithStatus("file change", item)
	case "mcpToolCall", "dynamicToolCall":
		return m.renderToolItem(item)
	case "contextCompaction":
		return "context compacted\n" + item.Text
	default:
		return itemWithStatus(item.Type, item)
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
	return itemWithStatus(label, item)
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

func terminalWidth(width int) int {
	return max(1, width-1)
}
