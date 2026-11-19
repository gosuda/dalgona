package tui

import (
	"fmt"
	"slices"
	"strings"
	"unicode"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
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

var pasteLineBreaks = strings.NewReplacer("\r\n", "\n", "\r", "\n")

func (f *textField) pasteLine(text string) bool {
	first, _, _ := strings.Cut(pasteLineBreaks.Replace(text), "\n")
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
		start := prevClusterRuneIndex(f.text, f.cursor)
		f.text = slices.Delete(f.text, start, f.cursor)
		f.cursor = start
		return true
	case "delete":
		if f.cursor == len(f.text) {
			return false
		}
		end := nextClusterRuneIndex(f.text, f.cursor)
		f.text = slices.Delete(f.text, f.cursor, end)
		return true
	case "left":
		f.cursor = prevClusterRuneIndex(f.text, f.cursor)
	case "right":
		f.cursor = nextClusterRuneIndex(f.text, f.cursor)
	}
	return false
}

func prevClusterRuneIndex(text []rune, cursor int) int {
	if cursor <= 0 {
		return 0
	}
	prefix := string(text[:cursor])
	pos, last := 0, 0
	for pos < len(prefix) {
		cluster, _ := ansi.FirstGraphemeCluster(prefix[pos:], ansi.GraphemeWidth)
		if len(cluster) == 0 {
			break
		}
		last = len(cluster)
		pos += last
	}
	if last == 0 {
		return cursor - 1
	}
	return cursor - utf8.RuneCountInString(prefix[pos-last:pos])
}

func nextClusterRuneIndex(text []rune, cursor int) int {
	if cursor >= len(text) {
		return len(text)
	}
	cluster, _ := ansi.FirstGraphemeCluster(string(text[cursor:]), ansi.GraphemeWidth)
	return cursor + utf8.RuneCountInString(cluster)
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
	text         []rune
	cursor       int
	width        int
	focused      bool
	placeholder  string
	chips        []chip
	verbatimText []rune
}

func newEditor() editor {
	return editor{width: 80, focused: true, placeholder: placeholderText}
}

type editorRow struct {
	text            string
	start, end, col int
}

func (c editor) Value() string {
	return string(c.text)
}

func (c *editor) SetValue(value string) {
	c.text = []rune(value)
	c.cursor = len(c.text)
	c.chips = nil
	c.verbatimText = nil
}

func (c *editor) recall(text string, verbatim bool) {
	c.SetValue(text)
	if verbatim {
		c.verbatimText = slices.Clone(c.text)
	}
}

func (c editor) verbatim() bool {
	return len(c.verbatimText) > 0 && slices.Equal(c.text, c.verbatimText)
}

func (c *editor) settleVerbatim() {
	if !c.verbatim() {
		c.verbatimText = nil
	}
}

func (c editor) readsSlash() bool {
	return !c.verbatim() && strings.HasPrefix(strings.TrimSpace(c.Value()), "/")
}

func (c editor) sendsPastedSlash(text string) bool {
	return !c.readsSlash() && strings.HasPrefix(text, "/")
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
	ranges := s.wrapRanges(line, cell)
	rows := make([]editorRow, len(ranges))
	for i, r := range ranges {
		rows[i] = editorRow{text: line[r.start:r.end], start: offset + r.start, end: offset + r.end, col: r.col}
	}
	return rows
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
	return s.cellsFrom(row.text[:n], row.col)
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
		lines[i] = s.expandFrom(norm.NFC.String(markKeywords(r.text, localSpans(spans, r), t)), r.col)
	}
	if c.Value() == "" {
		lines[0] = t.paint(roleDim, s.fit(c.placeholder, editorCells(width)))
	}
	return lines, row, min(c.cellsBefore(s, rows[row], at), editorCells(width))
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
	c.settleVerbatim()
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
	offset := 0
	for offset < len(row.text) {
		cluster, _ := ansi.FirstGraphemeCluster(row.text[offset:], s.method)
		if len(cluster) == 0 {
			break
		}
		next := offset + len(cluster)
		if s.cellsFrom(row.text[:next], row.col) > col {
			break
		}
		offset = next
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
	start := prevClusterRuneIndex(c.text, c.cursor)
	c.text = append(c.text[:start], c.text[c.cursor:]...)
	c.cursor = start
}

func (c *editor) delete() {
	if c.cursor >= len(c.text) {
		return
	}
	end := nextClusterRuneIndex(c.text, c.cursor)
	c.text = append(c.text[:c.cursor], c.text[end:]...)
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
	c.cursor = prevClusterRuneIndex(c.text, c.cursor)
}

func (c *editor) moveRight() {
	c.cursor = nextClusterRuneIndex(c.text, c.cursor)
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
	if r > 0xFFFFD {
		return chip{}, false
	}
	index := int(r) - chipRuneBase - 1
	if index < 0 || index >= len(c.chips) {
		return chip{}, false
	}
	return c.chips[index], true
}

func (c editor) hasTypedText() bool {
	return slices.ContainsFunc(c.text, func(r rune) bool {
		_, isChip := c.chipOf(r)
		return !isChip
	})
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
	text = sanitizeText(pasteLineBreaks.Replace(text))
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
	case key == "esc" && !m.input.composer.hasTypedText():
		m.leaveShell()
		return nil, true
	}
	return nil, false
}
