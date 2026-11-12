package tui

import (
	"slices"
	"strings"
	"unicode"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"golang.org/x/text/unicode/norm"
)

const (
	placeholderText = "Send a prompt · / commands · ? keys"
	promptText      = "› "
	promptCells     = 2
)

type inputMode int

const (
	inputPrompt inputMode = iota
	inputShell
	inputFilter
	inputTarget
	inputAnswer
	inputAPIKey
)

type textField struct {
	text   []rune
	cursor int
}

func (f textField) value() string {
	return string(f.text)
}

func (f *textField) pasteLine(text string) bool {
	first, _, _ := strings.Cut(text, "\n")
	return f.insert(first)
}

func (f *textField) insert(text string) bool {
	runes := []rune(sanitizeText(text))
	if len(runes) == 0 {
		return false
	}
	f.text = slices.Insert(f.text, f.cursor, runes...)
	f.cursor += len(runes)
	return true
}

func (f *textField) edit(key string) bool {
	switch key {
	case "backspace":
		if f.cursor == 0 {
			return false
		}
		f.text = slices.Delete(f.text, f.cursor-1, f.cursor)
		f.cursor--
		return true
	case "delete":
		if f.cursor == len(f.text) {
			return false
		}
		f.text = slices.Delete(f.text, f.cursor, f.cursor+1)
		return true
	case "left":
		f.cursor = max(0, f.cursor-1)
	case "right":
		f.cursor = min(len(f.text), f.cursor+1)
	}
	return false
}

func inputLabel(mode inputMode) string {
	switch mode {
	case inputPrompt:
		return promptText
	case inputShell:
		return "! "
	case inputFilter:
		return "filter: "
	case inputTarget:
		return "target: "
	case inputAnswer:
		return "answer: "
	case inputAPIKey:
		return "API key: "
	}
	return promptText
}

type editor struct {
	text        []rune
	cursor      int
	width       int
	focused     bool
	placeholder string
}

func newEditor() editor {
	return editor{width: 80, focused: true, placeholder: placeholderText}
}

type editorRow struct {
	text       string
	start, end int
}

func (c editor) Value() string {
	return string(c.text)
}

func (c *editor) SetValue(value string) {
	c.text = []rune(value)
	c.cursor = len(c.text)
}

func (c *editor) SetWidth(width int) {
	c.width = width
}

func (c *editor) Focus() tea.Cmd {
	c.focused = true
	return nil
}

func editorCells(width int) int {
	return max(1, terminalWidth(width)-promptCells)
}

func (c editor) rows(s *screenState, width int) []editorRow {
	var rows []editorRow
	offset := 0
	for line := range strings.SplitSeq(c.Value(), "\n") {
		rows = append(rows, c.lineRows(s, line, offset, editorCells(width))...)
		offset += len(line) + 1
	}
	return rows
}

func (c editor) lineRows(s *screenState, line string, offset, cell int) []editorRow {
	parts := s.wrap(line, cell)
	rows := make([]editorRow, 0, len(parts))
	pos := 0
	for _, part := range parts {
		end := partEnd(line, pos, part)
		rows = append(rows, editorRow{text: line[pos:end], start: offset + pos, end: offset + end})
		pos = end
	}
	return rows
}

func partEnd(line string, pos int, part string) int {
	if strings.HasPrefix(line[pos:], part) {
		return pos + len(part)
	}
	for end := pos + 1; end <= len(line); end++ {
		if end == len(line) || utf8.RuneStart(line[end]) {
			if norm.NFC.String(line[pos:end]) == part {
				return end
			}
		}
	}
	return len(line)
}

func (c editor) byteCursor() int {
	return len(string(c.text[:c.cursor]))
}

func caretRow(rows []editorRow, at int) int {
	chosen := 0
	for i, row := range rows {
		if row.start > at {
			break
		}
		chosen = i
		if at <= row.end {
			return i
		}
	}
	return chosen
}

func (c editor) cellsBefore(s *screenState, row editorRow, at int) int {
	n := min(max(at-row.start, 0), len(row.text))
	return s.cells(row.text[:n])
}

func localSpans(spans []keywordSpan, row editorRow) []keywordSpan {
	var out []keywordSpan
	for _, span := range spans {
		start, end := max(span.start, row.start), min(span.end, row.end)
		if start < end {
			out = append(out, keywordSpan{start: start - row.start, end: end - row.start})
		}
	}
	return out
}

func (c editor) layout(s *screenState, width int, spans []keywordSpan, t theme) ([]string, int, int) {
	rows := c.rows(s, width)
	at := c.byteCursor()
	row := caretRow(rows, at)
	lines := make([]string, len(rows))
	for i, r := range rows {
		lines[i] = norm.NFC.String(markKeywords(r.text, localSpans(spans, r), t))
	}
	if c.Value() == "" {
		lines[0] = t.paint(roleDim, s.fit(c.placeholder, editorCells(width)))
	}
	return lines, row, c.cellsBefore(s, rows[row], at)
}

func (c editor) onFirstRow(s *screenState) bool {
	return caretRow(c.rows(s, c.width), c.byteCursor()) == 0
}

func (c editor) onLastRow(s *screenState) bool {
	rows := c.rows(s, c.width)
	return caretRow(rows, c.byteCursor()) == len(rows)-1
}

func (c *editor) continueLine() bool {
	if c.cursor == 0 || c.text[c.cursor-1] != '\\' {
		return false
	}
	c.text[c.cursor-1] = '\n'
	return true
}

func (c editor) Update(s *screenState, msg tea.Msg) (editor, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.PasteMsg:
		c.insert([]rune(sanitizeText(msg.Content)))
	case tea.KeyPressMsg:
		c.updateKey(s, msg)
	}
	return c, nil
}

func (c *editor) updateKey(s *screenState, msg tea.KeyPressMsg) {
	key := msg.String()
	if c.editKey(key) || c.navigate(s, key) {
		return
	}
	if msg.Text != "" {
		c.insert([]rune(sanitizeText(msg.Text)))
	}
}

func (c *editor) editKey(key string) bool {
	switch key {
	case "backspace":
		c.backspace()
	case "delete":
		c.delete()
	case "shift+enter", "ctrl+j":
		c.insert([]rune{'\n'})
	case "ctrl+u":
		c.killLineStart()
	case "ctrl+k":
		c.killLineEnd()
	case "ctrl+w":
		c.deleteWord()
	default:
		return false
	}
	return true
}

func (c *editor) navigate(s *screenState, key string) bool {
	switch key {
	case "left":
		c.moveLeft()
	case "right":
		c.moveRight()
	case "up":
		c.moveRow(s, -1)
	case "down":
		c.moveRow(s, 1)
	default:
		return c.jump(key)
	}
	return true
}

func (c *editor) jump(key string) bool {
	switch key {
	case "home", "ctrl+a":
		c.cursor = c.lineStart()
	case "end", "ctrl+e":
		c.cursor = c.lineEnd()
	case "alt+left":
		c.cursor = c.wordStart()
	case "alt+right":
		c.cursor = c.wordEnd()
	default:
		return false
	}
	return true
}

func (c *editor) moveRow(s *screenState, delta int) {
	rows := c.rows(s, c.width)
	at := c.byteCursor()
	row := caretRow(rows, at)
	target := row + delta
	if target < 0 || target >= len(rows) {
		return
	}
	c.cursor = c.runeAt(s, rows[target], c.cellsBefore(s, rows[row], at))
}

func (c editor) runeAt(s *screenState, row editorRow, col int) int {
	offset, width := 0, 0
	for _, r := range row.text {
		cells := s.cells(string(r))
		if width+cells > col {
			break
		}
		width += cells
		offset += utf8.RuneLen(r)
	}
	return utf8.RuneCountInString(c.Value()[:row.start+offset])
}

func (c editor) lineStart() int {
	i := c.cursor
	for i > 0 && c.text[i-1] != '\n' {
		i--
	}
	return i
}

func (c editor) lineEnd() int {
	i := c.cursor
	for i < len(c.text) && c.text[i] != '\n' {
		i++
	}
	return i
}

func (c editor) wordStart() int {
	i := c.cursor
	for i > 0 && unicode.IsSpace(c.text[i-1]) {
		i--
	}
	for i > 0 && !unicode.IsSpace(c.text[i-1]) {
		i--
	}
	return i
}

func (c editor) wordEnd() int {
	i := c.cursor
	for i < len(c.text) && unicode.IsSpace(c.text[i]) {
		i++
	}
	for i < len(c.text) && !unicode.IsSpace(c.text[i]) {
		i++
	}
	return i
}

func (c *editor) insert(text []rune) {
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

func (c *editor) backspace() {
	if c.cursor == 0 {
		return
	}
	c.text = append(c.text[:c.cursor-1], c.text[c.cursor:]...)
	c.cursor--
}

func (c *editor) delete() {
	if c.cursor >= len(c.text) {
		return
	}
	c.text = append(c.text[:c.cursor], c.text[c.cursor+1:]...)
}

func (c *editor) killLineStart() {
	start := c.lineStart()
	c.text = append(c.text[:start], c.text[c.cursor:]...)
	c.cursor = start
}

func (c *editor) killLineEnd() {
	end := c.lineEnd()
	c.text = append(c.text[:c.cursor], c.text[end:]...)
}

func (c *editor) deleteWord() {
	start := c.wordStart()
	c.text = append(c.text[:start], c.text[c.cursor:]...)
	c.cursor = start
}

func (c *editor) moveLeft() {
	if c.cursor > 0 {
		c.cursor--
	}
}

func (c *editor) moveRight() {
	if c.cursor < len(c.text) {
		c.cursor++
	}
}
