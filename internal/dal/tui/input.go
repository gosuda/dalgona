package tui

import (
	"fmt"
	"slices"
	"sort"
	"strings"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"golang.org/x/text/unicode/norm"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

const (
	placeholderText = "Send a prompt · / commands · ? keys"
	promptText      = "› "
	promptCells     = 2
)

const shellPlaceholder = "Shell · enter runs · esc leaves"

const (
	pasteChipLines  = 10
	pasteLimit      = 1 << 20
	chipAtomBase    = utf8.MaxRune + 1
	chipPlaceholder = "\ufffc"
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
	limit  int
}

func (f textField) value() string {
	return string(f.text)
}

func (f *textField) insert(text string) error {
	if err := (textinput.Field{Name: "field", SingleLine: true}).Check(text); err != nil {
		return err
	}
	runes := []rune(text)
	if f.limit > 0 && len(f.text)+len(runes) > f.limit {
		return &textinput.Error{Field: "field", Reason: textinput.TextTooLarge}
	}
	f.text = slices.Insert(f.text, f.cursor, runes...)
	f.cursor += len(runes)
	return nil
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

func isChipAtom(r rune) bool { return r >= chipAtomBase }

func textSegment(text []rune, at int) (int, int) {
	start, end := at, at
	for start > 0 && !isChipAtom(text[start-1]) {
		start--
	}
	for end < len(text) && !isChipAtom(text[end]) {
		end++
	}
	return start, end
}

func prevClusterRuneIndex(text []rune, cursor int) int {
	if cursor <= 0 {
		return 0
	}
	if isChipAtom(text[cursor-1]) {
		return cursor - 1
	}
	start, _ := textSegment(text, cursor)
	prefix := string(text[start:cursor])
	return start + utf8.RuneCountInString(prefix[:textinput.PrevBoundary(prefix, len(prefix))])
}

func nextClusterRuneIndex(text []rune, cursor int) int {
	if cursor >= len(text) {
		return len(text)
	}
	if isChipAtom(text[cursor]) {
		return cursor + 1
	}
	start, end := textSegment(text, cursor)
	segment := string(text[start:end])
	at := len(string(text[start:cursor]))
	return cursor + utf8.RuneCountInString(segment[at:textinput.NextBoundary(segment, at)])
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
	chip            int
	source          []int
}

func (c editor) Value() string {
	return layoutText(c.text)
}

func layoutText(runes []rune) string {
	var out strings.Builder
	for _, r := range runes {
		if isChipAtom(r) {
			out.WriteString(chipPlaceholder)
			continue
		}
		out.WriteRune(r)
	}
	return out.String()
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
	return !c.verbatim() && slashStart(c.text) >= 0
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
	offset, start := 0, 0
	for i := 0; i <= len(c.text); i++ {
		if i < len(c.text) && c.text[i] != '\n' {
			continue
		}
		line := c.text[start:i]
		rows = append(rows, c.lineRows(s, line, offset, editorCells(width))...)
		offset += len(layoutText(line)) + 1
		start = i + 1
	}
	return rows
}

func (c editor) lineRows(s *screenState, line []rune, offset, cell int) []editorRow {
	var rows []editorRow
	start := 0
	for i, r := range line {
		if _, ok := c.chipOf(r); !ok {
			continue
		}
		if start < i {
			rows = append(rows, c.textRows(s, string(line[start:i]), offset, cell)...)
			offset += len(string(line[start:i]))
		}
		rows = append(rows, editorRow{start: offset, end: offset + len(chipPlaceholder), chip: int(r - chipAtomBase)})
		offset += len(chipPlaceholder)
		start = i + 1
	}
	if start < len(line) || len(rows) == 0 {
		rows = append(rows, c.textRows(s, string(line[start:]), offset, cell)...)
	}
	return rows
}

func (c editor) textRows(s *screenState, line string, offset, cell int) []editorRow {
	shown, source := editableText(line)
	ranges := s.wrapRanges(shown, cell)
	rows := make([]editorRow, len(ranges))
	for i, r := range ranges {
		marks := make([]int, r.end-r.start+1)
		for j := range marks {
			marks[j] = offset + source[r.start+j]
		}
		rows[i] = editorRow{text: shown[r.start:r.end], start: marks[0], end: marks[len(marks)-1], col: r.col, chip: -1, source: marks}
	}
	return rows
}

func editableText(line string) (string, []int) {
	var shown strings.Builder
	source := make([]int, 0, len(line)+1)
	for i, r := range line {
		piece := textinput.EditableRune(r)
		shown.WriteString(piece)
		if piece == string(r) {
			for j := range len(piece) {
				source = append(source, i+j)
			}
			continue
		}
		source = append(source, i)
		for range len(piece) - 1 {
			source = append(source, i+utf8.RuneLen(r))
		}
	}
	return shown.String(), append(source, len(line))
}

func (row editorRow) shownAt(at int) int {
	index := sort.Search(len(row.source), func(i int) bool { return row.source[i] > at }) - 1
	return min(max(index, 0), len(row.text))
}

func (c editor) byteCursor() int {
	return len(layoutText(c.text[:c.cursor]))
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
	return s.cellsFrom(row.text[:row.shownAt(at)], row.col)
}

func localSpans(spans []keywordSpan, row editorRow) []keywordSpan {
	var out []keywordSpan
	for _, span := range spans {
		start, end := max(span.start, row.start), min(span.end, row.end)
		if start < end && row.chip < 0 {
			out = append(out, keywordSpan{start: row.shownAt(start), end: row.shownAt(end)})
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
		c.insert([]rune(textinput.NewlinesToLF(msg.Text)))
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
	value := c.Value()
	source := value[row.start:row.end]
	offset := 0
	for offset < len(source) {
		next := textinput.NextBoundary(source, offset)
		if s.cellsFrom(row.text[:row.shownAt(row.start+next)], row.col) > col {
			break
		}
		offset = next
	}
	return utf8.RuneCountInString(value[:row.start+offset])
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
	for i > 0 && textinput.IsWhiteSpace(c.text[i-1]) {
		i--
	}
	for i > 0 && !textinput.IsWhiteSpace(c.text[i-1]) {
		i--
	}
	return i
}

func (c editor) wordEnd() int {
	i := c.cursor
	for i < len(c.text) && textinput.IsWhiteSpace(c.text[i]) {
		i++
	}
	for i < len(c.text) && !textinput.IsWhiteSpace(c.text[i]) {
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
	atom := rune(chipAtomBase)
	for range index {
		atom++
	}
	return atom
}

func chipLabel(number int, text string) string {
	return fmt.Sprintf("[Paste #%d +%d lines]", number, strings.Count(text, "\n")+1)
}

func (c editor) chipOf(r rune) (chip, bool) {
	index := int(r - chipAtomBase)
	if !isChipAtom(r) || index >= len(c.chips) {
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
	if row.chip < 0 {
		return chip{}, false
	}
	return c.chipOf(chipAtom(row.chip))
}

func (c editor) display(runes []rune) string {
	var out strings.Builder
	for _, r := range runes {
		if ch, ok := c.chipOf(r); ok {
			out.WriteString(ch.label)
			continue
		}
		out.WriteString(textinput.EditableRune(r))
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
	return c.expand(c.text)
}

func (c editor) expand(runes []rune) string {
	var out strings.Builder
	for _, r := range runes {
		if ch, ok := c.chipOf(r); ok {
			out.WriteString(ch.text)
			continue
		}
		out.WriteRune(r)
	}
	return out.String()
}

func (c *editor) paste(text string) {
	text = textinput.NewlinesToLF(text)
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
