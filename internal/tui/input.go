package tui

import (
	"fmt"
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

const shellPlaceholder = "Shell · enter runs · esc leaves"

const (
	pasteChipLines = 10
	pasteLimit     = 1 << 20
	chipRuneBase   = 0xF0000
)

type chip struct {
	label string
	text  string
	shell bool
	image string
	size  string
	bytes int
}

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
	chips       []chip
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
	c.chips = nil
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
	var rows []editorRow
	start := 0
	for i, r := range line {
		if _, ok := c.chipOf(r); !ok {
			continue
		}
		if start < i {
			rows = append(rows, c.textRows(s, line[start:i], offset+start, cell)...)
		}
		size := utf8.RuneLen(r)
		rows = append(rows, editorRow{text: line[i : i+size], start: offset + i, end: offset + i + size})
		start = i + size
	}
	if start < len(line) || len(rows) == 0 {
		rows = append(rows, c.textRows(s, line[start:], offset+start, cell)...)
	}
	return rows
}

func (c editor) textRows(s *screenState, line string, offset, cell int) []editorRow {
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
	if ch, ok := c.chipRow(row); ok {
		if at > row.start {
			return s.cells(ch.label)
		}
		return 0
	}
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
		if ch, ok := c.chipRow(r); ok {
			lines[i] = t.paint(roleText, s.fit(ch.label, editorCells(width)))
			continue
		}
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
		c.paste(msg.Content)
	case tea.KeyPressMsg:
		c.updateKey(s, msg)
	}
	c.prune()
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
	if _, ok := c.chipRow(row); ok {
		if col <= 0 {
			return utf8.RuneCountInString(c.Value()[:row.start])
		}
		return utf8.RuneCountInString(c.Value()[:row.end])
	}
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

func chipAtom(index int) rune {
	atom := rune(chipRuneBase)
	for range index + 1 {
		atom++
	}
	return atom
}

func chipLabel(number int, text string) string {
	return fmt.Sprintf("[Paste #%d +%d lines]", number, strings.Count(text, "\n")+1)
}

func (c editor) chipOf(r rune) (chip, bool) {
	index := int(r) - chipRuneBase - 1
	if index < 0 || index >= len(c.chips) {
		return chip{}, false
	}
	return c.chips[index], true
}

func (c editor) chipRow(row editorRow) (chip, bool) {
	r, size := utf8.DecodeRuneInString(row.text)
	if size == 0 || size != len(row.text) {
		return chip{}, false
	}
	return c.chipOf(r)
}

func (c editor) display(runes []rune) string {
	var out strings.Builder
	for _, r := range runes {
		if ch, ok := c.chipOf(r); ok {
			out.WriteString(ch.label)
			continue
		}
		out.WriteRune(r)
	}
	return out.String()
}

func (c editor) firstLine() string {
	line := c.text
	if i := slices.Index(line, '\n'); i >= 0 {
		line = line[:i]
	}
	return c.display(line)
}

func (c editor) Expanded() string {
	return c.expand(c.Value())
}

func (c editor) expand(text string) string {
	var out strings.Builder
	for _, r := range text {
		if ch, ok := c.chipOf(r); ok {
			out.WriteString(ch.text)
			continue
		}
		out.WriteRune(r)
	}
	return out.String()
}

func (c *editor) paste(text string) {
	text = sanitizeText(text)
	if strings.Count(text, "\n")+1 < pasteChipLines {
		c.insert([]rune(text))
		return
	}
	c.addChip(chip{label: chipLabel(len(c.chips)+1, text), text: text})
}

func (c *editor) addChip(ch chip) {
	c.chips = append(c.chips, ch)
	c.insert([]rune{chipAtom(len(c.chips) - 1)})
}

func (c *editor) prune() {
	inUse := make(map[rune]bool, len(c.chips))
	for _, r := range c.text {
		inUse[r] = true
	}
	renamed := make(map[rune]rune, len(c.chips))
	kept := make([]chip, 0, len(c.chips))
	for i, ch := range c.chips {
		atom := chipAtom(i)
		if !inUse[atom] {
			continue
		}
		renamed[atom] = chipAtom(len(kept))
		label := ch.label
		if ch.image != "" || !ch.shell {
			label = ch.tag(len(kept) + 1)
		}
		ch.label = label
		kept = append(kept, ch)
	}
	for i, r := range c.text {
		if next, ok := renamed[r]; ok {
			c.text[i] = next
		}
	}
	c.chips = kept
}

func (m *model) enterShell() {
	m.input.mode = inputShell
	m.input.composer.placeholder = shellPlaceholder
}

func (m *model) leaveShell() {
	m.input.mode = inputPrompt
	m.input.composer.placeholder = placeholderText
}

func (m *model) shellKey(key string, msg tea.KeyPressMsg) (tea.Cmd, bool) {
	switch {
	case m.input.mode == inputPrompt && msg.Text == "!" && m.input.composer.Value() == "":
		m.enterShell()
		return nil, true
	case m.input.mode != inputShell:
		return nil, false
	case key == "enter":
		return m.runShell(), true
	case key == "esc" && m.input.composer.Value() == "":
		m.leaveShell()
		return nil, true
	}
	return nil, false
}
