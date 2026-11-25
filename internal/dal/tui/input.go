package tui

import (
	"fmt"
	"slices"
	"strings"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
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
	chipPlaceholder = "\ufffc"
)

type chip struct {
	label string
	text  string
	shell bool
	image string
	size  string
	bytes int
	at    int
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

func (f textField) draft() textinput.Draft {
	at := len(string(f.text[:f.cursor]))
	return textinput.Draft{Text: string(f.text), Selection: textinput.Range{Start: at, End: at}}
}

func (f *textField) apply(draft textinput.Draft) {
	f.text = []rune(draft.Text)
	f.cursor = utf8.RuneCountInString(draft.Text[:draft.Selection.Start])
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
		return f.remove(textinput.Draft.DeleteBackward)
	case "delete":
		return f.remove(textinput.Draft.DeleteForward)
	case "left":
		f.step(false)
	case "right":
		f.step(true)
	}
	return false
}

func (f *textField) remove(op func(textinput.Draft) (textinput.Draft, error)) bool {
	before := f.draft()
	next, err := op(before)
	if err != nil || next == before {
		return false
	}
	f.apply(next)
	return true
}

func (f *textField) step(forward bool) {
	text := string(f.text)
	at := len(string(f.text[:f.cursor]))
	snapped, _ := textinput.PaintRange(text, textinput.Range{Start: at, End: at})
	var next int
	if forward {
		next, _ = textinput.NextClusterBoundary(text, snapped.Start)
	} else {
		next, _ = textinput.PreviousClusterBoundary(text, snapped.End)
	}
	f.cursor = utf8.RuneCountInString(text[:next])
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
	lead         int
	rowAffinity  int
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
	runeStart, chip int
}

func (c editor) Value() string {
	return string(c.text)
}

func (c editor) empty() bool {
	return len(c.text) == 0 && len(c.chips) == 0
}

func (c *editor) SetValue(value string) {
	c.text = []rune(value)
	c.cursor = len(c.text)
	c.lead, c.rowAffinity = 0, 0
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
	start := c.slashStart()
	return !c.verbatim() && start < len(c.text) && c.text[start] == '/' && !c.hasChipsAt(start)
}

func (c editor) sendsPastedSlash(text string) bool {
	return !c.readsSlash() && strings.HasPrefix(text, "/")
}

func (c *editor) SetWidth(width int) {
	if c.width != width {
		c.rowAffinity = 0
	}
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
	offset, ru, k := 0, 0, 0
	for line := range strings.SplitSeq(c.Value(), "\n") {
		lr := utf8.RuneCountInString(line)
		var idx []int
		for k < len(c.chips) && c.chips[k].at <= ru+lr {
			if c.chips[k].at >= ru {
				idx = append(idx, k)
			}
			k++
		}
		rows = append(rows, c.lineRows(s, lineLayout{line: line, offset: offset, cell: editorCells(width), ru: ru, idx: idx})...)
		offset += len(line) + 1
		ru += lr + 1
	}
	return rows
}

type lineLayout struct {
	line   string
	offset int
	cell   int
	ru     int
	idx    []int
}

func (c editor) lineRows(s *screenState, layout lineLayout) []editorRow {
	var rows []editorRow
	start, runeStart := 0, layout.ru
	for _, k := range layout.idx {
		at := c.chips[k].at
		byteAt := start + len(string(c.text[runeStart:at]))
		if start < byteAt {
			rows = append(rows, c.textRows(s, layout.line[start:byteAt], layout.offset+start, layout.cell, runeStart)...)
		}
		b := layout.offset + byteAt
		rows = append(rows, editorRow{start: b, end: b, runeStart: at, chip: k})
		start, runeStart = byteAt, at
	}
	if start < len(layout.line) || len(rows) == 0 {
		rows = append(rows, c.textRows(s, layout.line[start:], layout.offset+start, layout.cell, runeStart)...)
	}
	return rows
}

func (c editor) textRows(s *screenState, line string, offset, cell, runeStart int) []editorRow {
	ranges := s.wrapRanges(line, cell)
	rows := make([]editorRow, len(ranges))
	prev := 0
	for i, r := range ranges {
		runeStart += utf8.RuneCountInString(line[prev:r.start])
		rows[i] = editorRow{text: line[r.start:r.end], start: offset + r.start, end: offset + r.end, col: r.col, runeStart: runeStart, chip: -1}
		prev = r.start
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

func (c editor) caretRowOf(rows []editorRow) int {
	at := c.byteCursor()
	if i := c.rowAffinity - 1; i >= 0 && i < len(rows) && rows[i].start <= at && at <= rows[i].end {
		return i
	}
	if beside := c.chipBesideCaret(); beside >= 0 {
		if i := slices.IndexFunc(rows, func(r editorRow) bool { return r.chip == beside }); i >= 0 {
			return i
		}
	}
	return caretRow(rows, at)
}

func (c editor) chipBesideCaret() int {
	slot := c.caretSlot()
	if c.lead > 0 {
		return slot - 1
	}
	if slot < len(c.chips) && c.chips[slot].at == c.cursor {
		return slot
	}
	return -1
}

func (c editor) cellsBefore(s *screenState, row editorRow, at int) int {
	if ch, ok := c.chipRow(row); ok {
		if c.caretSlot() > row.chip {
			return s.cells(ch.label)
		}
		return 0
	}
	n := min(max(at-row.start, 0), len(row.text))
	return s.cellsFrom(displayText(row.text[:n]), row.col)
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

func (c editor) markedRow(s *screenState, text string, spans []keywordSpan, t theme) string {
	shown := displayText(text)
	if len(spans) == 0 {
		return shown
	}
	var mapped []keywordSpan
	for _, span := range spans {
		mapped = append(mapped, keywordSpan{start: len(displayText(text[:span.start])), end: len(displayText(text[:span.end]))})
	}
	return markKeywords(shown, mapped, t)
}

func (c editor) layout(s *screenState, width int, spans []keywordSpan, t theme) ([]string, int, int) {
	rows := c.rows(s, width)
	at := c.byteCursor()
	row := c.caretRowOf(rows)
	lines := make([]string, len(rows))
	for i, r := range rows {
		if ch, ok := c.chipRow(r); ok {
			lines[i] = t.paint(roleText, s.fit(ch.label, editorCells(width)))
			continue
		}
		lines[i] = s.expandFrom(s.nfc(c.markedRow(s, r.text, localSpans(spans, r), t)), r.col)
	}
	if c.Value() == "" && len(c.chips) == 0 {
		lines[0] = t.paint(roleDim, s.fit(c.placeholder, editorCells(width)))
	}
	return lines, row, min(c.cellsBefore(s, rows[row], at), editorCells(width))
}

func (c editor) onFirstRow(s *screenState) bool {
	return c.caretRowOf(c.rows(s, c.width)) == 0
}

func (c editor) onLastRow(s *screenState) bool {
	rows := c.rows(s, c.width)
	return c.caretRowOf(rows) == len(rows)-1
}

func (c *editor) continueLine() bool {
	if c.cursor == 0 || c.lead > 0 || c.text[c.cursor-1] != '\\' {
		return false
	}
	c.text[c.cursor-1] = '\n'
	c.repairCaret()
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
	if key != "up" && key != "down" {
		c.rowAffinity = 0
	}
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
		c.back(editor.previousStop)
	case "right":
		c.forward(editor.nextStop)
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
		c.cursor, c.lead = c.lineStart(), 0
	case "end", "ctrl+e":
		c.cursor = c.lineEnd()
		c.lead = c.chipsAt(c.cursor)
	case "alt+left":
		c.back(editor.wordStart)
	case "alt+right":
		c.forward(editor.wordEnd)
	default:
		return false
	}
	return true
}

func (c *editor) back(stop func(editor) int) {
	if c.lead > 0 {
		c.lead--
		return
	}
	if at := stop(*c); at < c.cursor {
		c.cursor, c.lead = at, c.chipsAt(at)
	}
}

func (c *editor) forward(stop func(editor) int) {
	if c.lead < c.chipsAt(c.cursor) {
		c.lead++
		return
	}
	if at := stop(*c); at > c.cursor {
		c.cursor, c.lead = at, 0
	}
}

func (c *editor) moveRow(s *screenState, delta int) {
	rows := c.rows(s, c.width)
	row := c.caretRowOf(rows)
	target := row + delta
	if target < 0 || target >= len(rows) {
		return
	}
	c.placeOnRow(s, rows[target], c.cellsBefore(s, rows[row], c.byteCursor()))
	c.rowAffinity = target + 1
}

func (c *editor) placeOnRow(s *screenState, row editorRow, col int) {
	if row.chip >= 0 {
		c.cursor = c.chips[row.chip].at
		c.lead = row.chip - c.chipIndex(c.cursor)
		if col > 0 {
			c.lead++
		}
		return
	}
	c.cursor = c.runeAt(s, row, col)
	c.lead = 0
	if c.cursor == row.runeStart {
		c.lead = c.chipsAt(c.cursor)
	}
}

func (c editor) runeAt(s *screenState, row editorRow, col int) int {
	offset, cells, runes := 0, 0, row.runeStart

	for offset < len(row.text) {
		size, _ := textinput.NextClusterBoundary(row.text[offset:], 0)
		cluster := row.text[offset : offset+size]
		cells += s.cellsFrom(displayText(cluster), row.col+cells)
		if cells > col {
			break
		}
		offset += size
		runes += utf8.RuneCountInString(cluster)
	}
	return runes
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

func (c editor) segmentStart() int {
	if i := c.chipIndex(c.cursor); i > 0 {
		return c.chips[i-1].at
	}
	return 0
}

func (c editor) segmentEnd() int {
	if i := c.chipIndex(c.cursor + 1); i < len(c.chips) {
		return c.chips[i].at
	}
	return len(c.text)
}

func (c editor) previousStop() int {
	start := c.segmentStart()
	head := string(c.text[start:c.cursor])
	stop, _ := textinput.PreviousClusterBoundary(head, len(head))
	return start + utf8.RuneCountInString(head[:stop])
}

func (c editor) nextStop() int {
	tail := string(c.text[c.cursor:c.segmentEnd()])
	stop, _ := textinput.NextClusterBoundary(tail, 0)
	return c.cursor + utf8.RuneCountInString(tail[:stop])
}

func (c editor) wordStart() int {
	start := c.segmentStart()
	head := string(c.text[start:c.cursor])
	word, at := 0, 0
	inWord := false
	for at < len(head) {
		if spaceCluster(head[at:]) {
			inWord = false
		} else if !inWord {
			word, inWord = at, true
		}
		size, _ := textinput.NextClusterBoundary(head[at:], 0)
		at += size
	}
	return start + utf8.RuneCountInString(head[:word])
}

func (c editor) wordEnd() int {
	tail := string(c.text[c.cursor:c.segmentEnd()])
	end := clustersForward(tail, clustersForward(tail, 0, spaceCluster), wordCluster)
	return c.cursor + utf8.RuneCountInString(tail[:end])
}

func clustersForward(text string, start int, kind func(string) bool) int {
	for start < len(text) {
		if !kind(text[start:]) {
			break
		}
		size, _ := textinput.NextClusterBoundary(text[start:], 0)
		start += size
	}
	return start
}

func spaceCluster(cluster string) bool {
	r, _ := utf8.DecodeRuneInString(cluster)
	return textinput.IsWhiteSpace(r)
}

func wordCluster(cluster string) bool {
	return !spaceCluster(cluster)
}

func (c editor) chipIndex(at int) int {
	for i, ch := range c.chips {
		if ch.at >= at {
			return i
		}
	}
	return len(c.chips)
}

func (c editor) chipsAt(at int) int {
	return c.chipIndex(at+1) - c.chipIndex(at)
}

func (c editor) hasChipsAt(at int) bool {
	return c.chipsAt(at) > 0
}

func (c editor) caretSlot() int {
	return c.chipIndex(c.cursor) + c.lead
}

func (c *editor) removeChips(first, end int) {
	if first >= end {
		return
	}
	c.chips = slices.Concat(c.chips[:first], c.chips[end:])
	c.verbatimText = nil
}

func (c *editor) cutText(start, end int) {
	c.text = slices.Concat(c.text[:start], c.text[end:])
	kept := make([]chip, 0, len(c.chips))
	for _, ch := range c.chips {
		if ch.at > start && ch.at < end {
			continue
		}
		if ch.at >= end {
			ch.at -= end - start
		}
		kept = append(kept, ch)
	}
	c.chips = kept
}

func (c *editor) insert(text []rune) {
	if len(text) == 0 {
		return
	}
	folded, ok := foldValidPaste(string(text))
	if !ok {
		return
	}
	runes := []rune(folded)
	slot := c.caretSlot()
	c.text = slices.Concat(c.text[:c.cursor], runes, c.text[c.cursor:])
	for i := range c.chips[slot:] {
		c.chips[slot+i].at += len(runes)
	}
	c.cursor += len(runes)
	c.lead = 0
	c.repairCaret()
}

func (c *editor) repairCaret() {
	c.rowAffinity = 0
	if c.cursor == 0 || c.cursor == len(c.text) || c.hasChipsAt(c.cursor) {
		return
	}
	start := c.segmentStart()
	text := string(c.text[start:c.segmentEnd()])
	before := len(string(c.text[start : c.cursor-1]))
	snapped, _ := textinput.PaintRange(text, textinput.Range{Start: before, End: before})
	next, _ := textinput.NextClusterBoundary(text, snapped.Start)
	c.cursor = start + utf8.RuneCountInString(text[:next])
}

func (c *editor) backspace() {
	if c.lead > 0 {
		slot := c.caretSlot()
		c.removeChips(slot-1, slot)
		c.lead--
		c.repairCaret()
		return
	}
	c.cutBefore(c.previousStop())
}

func (c *editor) cutBefore(start int) {
	if start == c.cursor {
		return
	}
	lead := c.chipsAt(start)
	c.cutText(start, c.cursor)
	c.cursor, c.lead = start, lead
	c.repairCaret()
}

func (c *editor) delete() {
	if slot := c.caretSlot(); c.lead < c.chipsAt(c.cursor) {
		c.removeChips(slot, slot+1)
		c.repairCaret()
		return
	}
	c.cutText(c.cursor, c.nextStop())
	c.repairCaret()
}

func (c *editor) killLineStart() {
	start := c.lineStart()
	c.removeChips(c.chipIndex(start), c.caretSlot())
	c.cutText(start, c.cursor)
	c.cursor, c.lead = start, 0
	c.repairCaret()
}

func (c *editor) killLineEnd() {
	end := c.lineEnd()
	c.removeChips(c.caretSlot(), c.chipIndex(end+1))
	c.cutText(c.cursor, end)
	c.repairCaret()
}

func (c *editor) deleteWord() {
	if c.lead > 0 {
		c.backspace()
		return
	}
	c.cutBefore(c.wordStart())
}

func chipLabel(number int, text string) string {
	return fmt.Sprintf("[Paste #%d +%d lines]", number, strings.Count(text, "\n")+1)
}

func (c editor) chipRow(row editorRow) (chip, bool) {
	if row.chip < 0 || row.chip >= len(c.chips) {
		return chip{}, false
	}
	return c.chips[row.chip], true
}

func chipShown(ch chip) string {
	return ch.label
}

func chipExpansion(ch chip) string {
	return ch.text
}

func (c editor) joined(start, end int, part func(chip) string) string {
	var out strings.Builder
	prev := start
	for _, ch := range c.chips {
		if ch.at < start || ch.at > end {
			continue
		}
		out.WriteString(string(c.text[prev:ch.at]))
		out.WriteString(part(ch))
		prev = ch.at
	}
	out.WriteString(string(c.text[prev:end]))
	return out.String()
}

func (c editor) display() string {
	return c.joined(0, len(c.text), chipShown)
}

func (c editor) firstLine() string {
	end := slices.Index(c.text, '\n')
	if end < 0 {
		end = len(c.text)
	}
	return displayText(c.joined(0, end, chipShown))
}

func (c editor) expandRange(start, end int) string {
	start, end = max(start, 0), min(end, len(c.text))
	if start > end {
		return ""
	}
	return c.joined(start, end, chipExpansion)
}

func (c editor) Expanded() string {
	return c.joined(0, len(c.text), chipExpansion)
}

func (c *editor) paste(text string) {
	folded, ok := foldValidPaste(text)
	if !ok {
		return
	}
	if strings.Count(folded, "\n")+1 < pasteChipLines {
		c.insert([]rune(folded))
		return
	}
	c.addChip(chip{label: chipLabel(len(c.chips)+1, folded), text: folded})
}

func (c *editor) addChip(ch chip) {
	ch.at = c.cursor
	slot := c.caretSlot()
	c.chips = slices.Concat(c.chips[:slot], []chip{ch}, c.chips[slot:])
	c.lead++
	c.rowAffinity = 0
	c.verbatimText = nil
	c.prune()
}

func (c *editor) prune() {
	ordered := slices.Clone(c.chips)
	slices.SortStableFunc(ordered, func(a, b chip) int { return a.at - b.at })
	kept := make([]chip, 0, len(ordered))
	for _, ch := range ordered {
		ch.at = min(max(ch.at, 0), len(c.text))
		label := ch.label
		if ch.image != "" || !ch.shell {
			label = ch.tag(len(kept) + 1)
		}
		ch.label = label
		kept = append(kept, ch)
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
	case m.input.mode == inputPrompt && msg.Text == "!" && m.input.composer.empty():
		m.enterShell()
		return nil, true
	case m.input.mode != inputShell:
		return nil, false
	case key == "enter":
		return m.runShell(), true
	case key == "esc" && len(m.input.composer.text) == 0:
		m.leaveShell()
		return nil, true
	}
	return nil, false
}
