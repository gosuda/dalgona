package tui

import (
	"cmp"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"slices"
	"strconv"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/rpc"
)

const (
	settleDelay     = 3 * time.Second / framesPerSecond
	maxSettledItems = 200
)

const maxReviewEntries = 1000

const (
	verbosityLow     = "low"
	verbosityMedium  = "medium"
	verbosityHigh    = "high"
	diffPreviewLines = 60
	outputTailLines  = 10
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
	key   string
	title string
	text  string
}

type liveItem struct {
	rpc.Item
	raw        []byte
	text       []byte
	output     []byte
	split      blockSplitter
	blocks     int
	streamTail string
	children   []string
}

func (l *liveItem) settle(final rpc.Item) rpc.Item {
	l.Text += string(l.text)
	l.Output += string(l.output)
	return mergeItem(final, l.Item)
}

func (l *liveItem) tail() string {
	switch l.Type {
	case "commandExecution":
		return lastLine(l.Output + string(l.output))
	case "agentMessage":
		return strings.TrimSpace(l.streamTail)
	}
	return lastLine(l.Text + string(l.text))
}

func (l liveItem) snapshot() rpc.Item {
	item := l.Item
	item.Text += string(l.text)
	item.Output += string(l.output)
	return item
}

func (l *liveItem) activity() string {
	tail := l.tail()
	if l.Type != "commandExecution" {
		return cmp.Or(tail, activityLabel(l.Item))
	}
	return strings.Join(slices.DeleteFunc([]string{activityLabel(l.Item), tail}, func(s string) bool { return s == "" }), " · ")
}

func lastLine(text string) string {
	lines := strings.Split(strings.TrimRight(text, "\n"), "\n")
	return strings.TrimSpace(lines[len(lines)-1])
}

func isAdvisorItem(item rpc.Item) bool {
	return item.Type == "dynamicToolCall" && item.Tool == "advisor"
}

func (m *model) commit(key string, lines []string) {
	m.commitFull(key, lines, lines)
}

func (m *model) commitFull(key string, lines, full []string) {
	if len(lines) == 0 || !m.keepEntry(key, full) {
		return
	}
	m.out.pending = append(m.out.pending, lines...)
}

func (m *model) keepEntry(key string, full []string) bool {
	if _, ok := m.out.printed[key]; ok {
		return false
	}
	m.out.printed[key] = struct{}{}
	m.out.entries = append(m.out.entries, newEntry(key, full))
	if extra := len(m.out.entries) - maxReviewEntries; extra > 0 {
		m.out.entries = slices.Clone(m.out.entries[extra:])
	}
	return true
}

func (s *screenState) level() string {
	return cmp.Or(s.verbosity, verbosityMedium)
}

func (m *model) settleItem(key string, item rpc.Item, children []string) {
	if _, seen := m.out.printed[key]; seen || silentTodo(item) {
		return
	}
	item = sanitizeItem(item)
	level := m.screen.level()
	if level == verbosityLow && groupedItem(item) {
		if m.keepEntry(key, m.renderSettled(item, children, verbosityHigh)) {
			m.live.group = append(m.live.group, item)
		}
		return
	}
	m.flushGroup()
	printed := m.renderSettled(item, children, level)
	full := printed
	if level != verbosityHigh && levelSensitive(item) {
		full = m.renderSettled(item, children, verbosityHigh)
	}
	m.commitFull(key, printed, full)
}

func silentTodo(item rpc.Item) bool {
	return item.Type == "dynamicToolCall" && item.Tool == "todo" && item.Status == "completed"
}

func groupedItem(item rpc.Item) bool {
	return item.Status == "completed" && isToolItem(item)
}

func levelSensitive(item rpc.Item) bool {
	return item.Type != "agentMessage" && item.Type != "userMessage"
}

func (m *model) flushGroup() {
	if len(m.live.group) == 0 {
		return
	}
	summary := groupSummary(m.live.group)
	m.live.group = nil
	m.live.groupN++
	m.commit(fmt.Sprintf("group:%s:%d", m.thread.turnID, m.live.groupN), []string{m.screen.theme.paint(roleDim, summary), ""})
}

func groupSummary(items []rpc.Item) string {
	var commands, files, calls int
	for _, item := range items {
		switch item.Type {
		case "commandExecution":
			commands++
		case "fileChange":
			files += max(1, len(item.Changes))
		default:
			calls++
		}
	}
	var parts []string
	if commands > 0 {
		parts = append(parts, "Ran "+pluralText(commands, "command"))
	}
	if files > 0 {
		parts = append(parts, "edited "+pluralText(files, "file"))
	}
	if calls > 0 {
		parts = append(parts, "called "+pluralText(calls, "tool"))
	}
	return strings.Join(parts, " · ")
}

func pluralText(n int, word string) string {
	if n == 1 {
		return "1 " + word
	}
	return strconv.Itoa(n) + " " + word + "s"
}

func newEntry(key string, lines []string) entry {
	text := strings.Join(lines, "\n")
	return entry{key: key, title: entryTitle(text), text: text}
}

func entryTitle(text string) string {
	first, _, _ := strings.Cut(text, "\n")
	return strings.TrimSpace(ansi.Strip(first))
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
	if m.out.printing || m.out.settling || m.review != nil || len(m.out.pending) == 0 {
		return nil
	}
	bound := m.screen.height - len(m.dockLines())
	if bound < 1 {
		return nil
	}
	text, ok := m.nextChunk(bound)
	if !ok {
		return nil
	}
	m.out.frozen = m.View()
	m.out.printing = true
	printed := func() tea.Msg { return printedMsg{} }
	return tea.Sequence(tea.Println(text), printed)
}

func (m *model) nextChunk(bound int) (string, bool) {
	rows := m.takeChunk(bound)
	if len(rows) == 0 {
		return "", false
	}
	return m.screen.emit(chunkText(rows)), true
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

func (m *model) renderReviewRow(verdict, summary string, paths []string) []string {
	s := &m.screen
	cleaned := make([]string, len(paths))
	for i, path := range paths {
		cleaned[i] = sanitizeText(path)
	}
	text := "review  " + sanitizeText(summary)
	if len(cleaned) > 0 {
		text += "  " + strings.Join(cleaned, ", ")
	}
	var out []string
	for _, line := range s.wrap(text, terminalWidth(s.width)) {
		out = append(out, reviewPaint(s.theme, verdict, line))
	}
	return out
}

func reviewPaint(t theme, verdict, line string) string {
	switch verdict {
	case "error":
		return t.paintBold(roleFailedText, line)
	case "flagged":
		return t.paint(roleWarningText, line)
	}
	return t.paint(roleDim, line)
}

func (m *model) printReview(key, verdict, summary string, paths []string) {
	if verdict == "clean" || (verdict == "uncertain" && m.screen.level() != verbosityHigh) {
		return
	}
	m.commit(key, m.renderReviewRow(verdict, summary, paths))
}

func (m *model) renderAdvisorCard(advisor, severity, note string) string {
	advisor, severity, note = sanitizeText(advisor), sanitizeText(severity), sanitizeText(note)
	label := "advisor"
	if advisor != "" && advisor != label {
		label += " " + advisor
	}
	return m.screen.theme.paint(roleDim, label+" · "+severity+": "+note)
}

const (
	rowLeadCells     = 2
	rowStateCells    = 8
	rowLeadWidth     = rowLeadCells + rowStateCells
	rowLabelCells    = 9
	rowMetaCells     = 9
	rowDurationCells = 8
	rowRightCells    = rowMetaCells + rowDurationCells
	evalTool         = "eval"
)

type row struct {
	mark     string
	label    string
	subject  string
	state    string
	meta     string
	duration string
	body     []string
}

func (m *model) renderRow(r row) []string {
	s := &m.screen
	width := terminalWidth(s.width)
	label := m.rowLabel(r.label)
	lead := rowLeadWidth + s.cells(label)
	subjectWidth := max(0, width-lead-rowRightCells)
	line := s.pad(r.mark, rowLeadCells) + m.rowState(r.state) + label +
		s.pad(s.fit(r.subject, subjectWidth), subjectWidth) +
		s.pad(s.theme.paint(roleDim, s.fit(r.meta, rowMetaCells)), rowMetaCells) +
		s.padLeft(s.theme.paint(roleDim, s.fit(r.duration, rowDurationCells)), rowDurationCells)
	out := []string{strings.TrimRight(s.fit(line, width), " ")}
	bodyWidth := max(1, width-lead)
	for _, text := range r.body {
		out = append(out, m.rowBody(text, lead, bodyWidth)...)
	}
	return append(out, "")
}

func (m *model) rowBody(text string, indent, width int) []string {
	var out []string
	for _, part := range m.screen.wrap(text, width) {
		if part == "" {
			out = append(out, "")
			continue
		}
		out = append(out, strings.Repeat(" ", indent)+part)
	}
	return out
}

func (m *model) rowLabel(label string) string {
	if label == "" {
		return ""
	}
	return m.screen.theme.paintBold(roleText, m.screen.fit(label, rowLabelCells)) + " "
}

func (m *model) rowState(word string) string {
	if word == "" {
		return strings.Repeat(" ", rowStateCells)
	}
	return m.screen.pad(m.stateText(m.screen.fit(word, rowStateCells)), rowStateCells)
}

func (m *model) stateText(word string) string {
	t := m.screen.theme
	switch word {
	case "Failed":
		return t.paintBold(roleFailedText, word)
	case "Working":
		return t.paint(roleText, word)
	}
	return t.paint(roleDim, word)
}

func statusWord(status string) string {
	switch status {
	case "completed":
		return "Done"
	case "failed":
		return "Failed"
	case "declined":
		return "Declined"
	case "inProgress":
		return "Working"
	}
	return status
}

func textLines(text string) []string {
	if text == "" {
		return nil
	}
	return strings.Split(strings.TrimRight(text, "\n"), "\n")
}

func firstLine(text string) string {
	line, _, _ := strings.Cut(text, "\n")
	return line
}

func durationText(ms *int64) string {
	if ms == nil {
		return ""
	}
	return formatElapsed(time.Duration(*ms) * time.Millisecond)
}

func itemBody(item rpc.Item) []string {
	return append(textLines(item.Text), textLines(item.Output)...)
}

func commandRow(item rpc.Item, level string) row {
	r := row{mark: "$", subject: firstLine(item.Command), state: statusWord(item.Status), duration: durationText(item.DurationMs), body: levelBody(item, textLines(item.Output), level)}
	if item.ExitCode != nil {
		r.meta = fmt.Sprintf("exit %d", *item.ExitCode)
	}
	return r
}

func changeLabel(kind string) string {
	switch kind {
	case "add":
		return "Created"
	case "delete":
		return "Deleted"
	case "update":
		return "Edited"
	}
	return "Changed"
}

func (m *model) changeRow(change rpc.FileChange, state, level string) row {
	r := row{label: changeLabel(change.Kind), subject: change.Path, state: state}
	if change.DiffOmitted {
		r.meta = "diff omitted"
		r.body = []string{"diff omitted, larger than 1 MiB"}
		return r
	}
	r.meta = fmt.Sprintf("+%d −%d", change.Additions, change.Deletions)
	r.body = m.diffBody(change.Diff, level)
	return r
}

func (m *model) diffBody(diff, level string) []string {
	lines := textLines(diff)
	cut := 0
	if level != verbosityHigh && len(lines) > diffPreviewLines {
		cut = len(lines) - diffPreviewLines
		lines = lines[:diffPreviewLines]
	}
	index := slices.IndexFunc(lines, func(line string) bool { return strings.HasPrefix(line, "@@") })
	if index < 0 {
		index = len(lines)
	}
	t := m.screen.theme
	out := make([]string, 0, len(lines)+1)
	for _, line := range lines[:index] {
		out = append(out, t.paint(roleDim, line))
	}
	for _, line := range lines[index:] {
		out = append(out, m.diffLine(line))
	}
	if cut > 0 {
		out = append(out, t.paint(roleDim, fmt.Sprintf("… %d more lines · ctrl+o", cut)))
	}
	return out
}

func (m *model) diffLine(line string) string {
	t := m.screen.theme
	switch {
	case strings.HasPrefix(line, "@@"):
		return t.paint(roleDim, line)
	case strings.HasPrefix(line, "+"):
		return t.paint(roleAddedText, line)
	case strings.HasPrefix(line, "-"):
		return t.paint(roleFailedText, line)
	}
	return line
}

func (m *model) renderFileChange(item rpc.Item, level string) []string {
	state := statusWord(item.Status)
	if len(item.Changes) == 0 {
		return m.renderRow(row{label: "File change", state: state})
	}
	var out []string
	if len(item.Changes) > 1 {
		out = append(out, fmt.Sprintf("Changed %d files", len(item.Changes)))
	}
	for _, change := range item.Changes {
		diffLevel := level
		if item.Status == "failed" {
			diffLevel = verbosityHigh
		}
		r := m.changeRow(change, state, diffLevel)
		r.body = levelBody(item, r.body, level)
		out = append(out, m.renderRow(r)...)
	}
	return out
}

func toolLabel(tool string) string {
	if tool == "" {
		return "Tool"
	}
	return strings.ToUpper(tool[:1]) + tool[1:]
}

func toolSubject(item rpc.Item) string {
	var args struct {
		Path    string `json:"path"`
		Pattern string `json:"pattern"`
		URL     string `json:"url"`
		Prompt  string `json:"prompt"`
		Code    string `json:"code"`
	}
	_ = json.Unmarshal(item.Arguments, &args)
	for _, value := range []string{args.Path, args.Pattern, args.URL, args.Prompt, args.Code} {
		if value != "" {
			return firstLine(sanitizeText(value))
		}
	}
	return ""
}

func (m *model) advisorCard(item rpc.Item) (string, bool) {
	var args struct {
		Advisor  string `json:"advisor"`
		Severity string `json:"severity"`
	}
	if !isAdvisorItem(item) || json.Unmarshal(item.Arguments, &args) != nil {
		return "", false
	}
	return m.renderAdvisorCard(args.Advisor, args.Severity, item.Output), true
}

func (m *model) evalLines(item rpc.Item, level string) []string {
	var args struct {
		Code string `json:"code"`
	}
	decoded := json.Unmarshal(item.Arguments, &args) == nil
	if level == verbosityHigh {
		subject := ""
		if decoded {
			subject = firstLine(sanitizeText(args.Code))
		}
		return m.renderRow(row{label: "Eval", subject: subject, state: statusWord(item.Status), duration: durationText(item.DurationMs), body: levelBody(item, itemBody(item), level)})
	}
	text := "Eval"
	if decoded {
		count := len(textLines(sanitizeText(args.Code)))
		noun := "lines of code"
		if count == 1 {
			noun = "line of code"
		}
		text += fmt.Sprintf(" · %d %s", count, noun)
	}
	if word := statusWord(item.Status); word != "" {
		text += " · " + m.stateText(word)
	}
	out := []string{text}
	if item.Status == "failed" {
		out = append(out, m.failedTail(itemBody(item))...)
	}
	return append(out, "")
}

func (m *model) failedTail(lines []string) []string {
	width := max(1, terminalWidth(m.screen.width)-rowLeadWidth)
	var out []string
	for _, line := range tailLines(lines, outputTailLines) {
		out = append(out, m.rowBody(line, rowLeadWidth, width)...)
	}
	return out
}

func tailLines(lines []string, keep int) []string {
	extra := len(lines) - keep
	if extra <= 0 {
		return lines
	}
	return append([]string{fmt.Sprintf("… %d earlier lines", extra)}, lines[extra:]...)
}

func levelBody(item rpc.Item, lines []string, level string) []string {
	if item.Status == "failed" || (level != verbosityHigh && item.Type == "commandExecution") {
		return tailLines(lines, outputTailLines)
	}
	return lines
}

func (m *model) renderDynamic(item rpc.Item, level string) []string {
	if card, ok := m.advisorCard(item); ok {
		return append(textLines(card), "")
	}
	if silentTodo(item) {
		return nil
	}
	if item.Tool == evalTool {
		return m.evalLines(item, level)
	}
	if item.Tool == subagentTool {
		return m.renderSubagent(item, nil, level)
	}
	return m.renderRow(row{label: toolLabel(item.Tool), subject: toolSubject(item), state: statusWord(item.Status), duration: durationText(item.DurationMs), body: levelBody(item, itemBody(item), level)})
}

func (m *model) renderItem(item rpc.Item, level string) []string {
	item = sanitizeItem(item)
	switch item.Type {
	case "userMessage":
		mark := m.screen.theme.hueText(threadHue(m.thread.id), "›")
		return m.renderRow(row{mark: mark, body: textLines(markKeywords(item.Text, spansOf(item.Keywords), m.screen.theme))})
	case "agentMessage":
		if strings.TrimSpace(item.Text) == "" {
			return nil
		}
		return append(textLines(m.renderMarkdown(item.Text)), "")
	case "commandExecution":
		return m.renderRow(commandRow(item, level))
	case "fileChange":
		return m.renderFileChange(item, level)
	case "dynamicToolCall":
		return m.renderDynamic(item, level)
	default:
		return m.renderOtherItem(item, level)
	}
}

const subagentTool = "task"

func (m *model) attachChild(id string) {
	for i := len(m.live.items) - 1; i >= 0; i-- {
		item := &m.live.items[i]
		if item.Type == "dynamicToolCall" && item.Tool == subagentTool && item.Status == "inProgress" {
			item.children = append(item.children, id)
			return
		}
	}
}

func (m *model) renderSettled(item rpc.Item, children []string, level string) []string {
	if item.Type == "dynamicToolCall" && item.Tool == subagentTool {
		return m.renderSubagent(item, children, level)
	}
	return m.renderItem(item, level)
}

func (m *model) renderSubagent(item rpc.Item, children []string, level string) []string {
	body := make([]string, 0, len(children)+1)
	for _, id := range children {
		body = append(body, "child "+id+" · /resume "+id)
	}
	body = append(body, subagentResult(item)...)
	return m.renderRow(row{label: "Subagent", subject: toolSubject(item), state: statusWord(item.Status), duration: durationText(item.DurationMs), body: levelBody(item, body, level)})
}

func subagentResult(item rpc.Item) []string {
	var result struct {
		Data  jsontext.Value `json:"data"`
		Error string         `json:"error"`
	}
	text := cmp.Or(item.Output, item.Text)
	if json.Unmarshal([]byte(text), &result) != nil {
		return itemBody(item)
	}
	if result.Error != "" {
		return []string{"error: " + firstLine(sanitizeText(result.Error))}
	}
	if len(result.Data) == 0 {
		return nil
	}
	return []string{firstLine(sanitizeText(subagentData(result.Data)))}
}

func subagentData(data jsontext.Value) string {
	var text string
	if json.Unmarshal(data, &text) == nil {
		return text
	}
	return string(data)
}

func (m *model) renderOtherItem(item rpc.Item, level string) []string {
	switch item.Type {
	case "reasoning":
		if level == verbosityHigh {
			return m.renderRow(row{label: "Thinking", body: textLines(item.Text)})
		}
		return m.thinkingLine(item.Text)
	case "mcpToolCall":
		return m.renderRow(row{label: "Called", subject: item.Server + "/" + item.Tool, state: statusWord(item.Status), duration: durationText(item.DurationMs), body: levelBody(item, itemBody(item), level)})
	case "contextCompaction":
		return m.renderRow(row{label: "Compacted context", body: textLines(item.Text)})
	}
	return m.renderRow(row{label: item.Type, state: statusWord(item.Status), body: levelBody(item, itemBody(item), level)})
}

func (m *model) thinkingLine(text string) []string {
	lines := textLines(text)
	if len(lines) == 0 {
		return nil
	}
	return []string{m.screen.theme.paint(roleDim, "Thinking · "+pluralText(len(lines), "line")+" · ctrl+o"), ""}
}

func terminalWidth(width int) int {
	return max(1, width-1)
}
