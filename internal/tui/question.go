package tui

import (
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"math/bits"
	"slices"
	"strings"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

const (
	stateAwaitingInput  = "awaiting input"
	questionAnswerLimit = 2000
)

type cardAction int

const (
	cardStay cardAction = iota
	cardSubmit
	cardDismiss
)

type questionCard struct {
	threadID string
	params   rpc.ToolRequestUserInputParams
	answer   chan requestAnswer
	index    int
	cursor   []int
	picked   [][]bool
	typed    []string
	typing   bool
}

func newQuestionCard(params rpc.ToolRequestUserInputParams, answer chan requestAnswer) *questionCard {
	for i := range params.Questions {
		q := &params.Questions[i]
		q.Header = sanitizeText(q.Header)
		q.Question = sanitizeText(q.Question)
		for j := range q.Options {
			o := &q.Options[j]
			o.Label = sanitizeText(o.Label)
			o.Description = sanitizeText(o.Description)
			o.Preview = sanitizeText(o.Preview)
		}
	}
	card := &questionCard{threadID: params.ThreadID, params: params, answer: answer}
	card.cursor = make([]int, len(params.Questions))
	card.typed = make([]string, len(params.Questions))
	card.picked = make([][]bool, len(params.Questions))
	for i, q := range params.Questions {
		card.picked[i] = make([]bool, len(q.Options))
	}
	card.typing = card.freeText()
	return card
}

func (c *questionCard) freeText() bool {
	return len(c.params.Questions[c.index].Options) == 0
}

func (c *questionCard) rows(q int) int {
	n := len(c.params.Questions[q].Options)
	if c.params.Questions[q].IsOther {
		n++
	}
	return n
}

func (c *questionCard) key(msg tea.KeyPressMsg) cardAction {
	return c.rowsKey(msg.String())
}

func (c *questionCard) rowsKey(key string) cardAction {
	if c.navigate(key) {
		return cardStay
	}
	return c.choose(key)
}

func (c *questionCard) navigate(key string) bool {
	switch key {
	case "up", "k":
		c.cursor[c.index] = max(0, c.cursor[c.index]-1)
	case "down", "j":
		c.cursor[c.index] = min(c.rows(c.index)-1, c.cursor[c.index]+1)
	case "tab", "right", "l":
		c.moveQuestion(1)
	case "shift+tab", "left", "h":
		c.moveQuestion(-1)
	default:
		return false
	}
	return true
}

func (c *questionCard) moveQuestion(delta int) {
	c.index = min(max(c.index+delta, 0), len(c.params.Questions)-1)
	c.typing = c.freeText()
}

func (c *questionCard) choose(key string) cardAction {
	switch key {
	case "s":
		return cardSubmit
	case "esc":
		return cardDismiss
	case "enter":
		return c.enter()
	case "space":
		return c.toggle(c.cursor[c.index])
	}
	if n, ok := digitRow(key); ok {
		if n >= c.rows(c.index) {
			return cardStay
		}
		if c.multi() {
			return c.pick(n)
		}
		return c.pickAdvance(n)
	}
	return cardStay
}

func digitRow(key string) (int, bool) {
	if len(key) != 1 || key[0] < '1' || key[0] > '9' {
		return 0, false
	}
	return int(key[0] - '1'), true
}

func (c *questionCard) multi() bool {
	return c.params.Questions[c.index].MultiSelect
}

func (c *questionCard) enter() cardAction {
	if c.multi() {
		if c.cursor[c.index] >= len(c.params.Questions[c.index].Options) {
			c.typing = true
			return cardStay
		}
		return c.advance()
	}
	return c.pickAdvance(c.cursor[c.index])
}

func (c *questionCard) toggle(row int) cardAction {
	if !c.multi() {
		return c.pickAdvance(row)
	}
	return c.pick(row)
}

func (c *questionCard) pick(row int) cardAction {
	q := c.index
	c.cursor[q] = row
	if row >= len(c.params.Questions[q].Options) {
		c.typing = true
		return cardStay
	}
	if c.multi() {
		c.picked[q][row] = !c.picked[q][row]
		return cardStay
	}
	clear(c.picked[q])
	c.picked[q][row] = true
	c.typed[q] = ""
	return cardStay
}

func (c *questionCard) pickAdvance(row int) cardAction {
	if act := c.pick(row); c.typing || act != cardStay {
		return act
	}
	return c.advance()
}

func (c *questionCard) advance() cardAction {
	if c.index+1 >= len(c.params.Questions) {
		return cardSubmit
	}
	c.moveQuestion(1)
	return cardStay
}

func (c *questionCard) commitText(text string) cardAction {
	q := c.index
	runes := []rune(sanitizeText(text))
	if len(runes) > questionAnswerLimit {
		runes = runes[:questionAnswerLimit]
	}
	c.typed[q] = string(runes)
	if !c.multi() {
		clear(c.picked[q])
	}
	c.typing = false
	return c.advance()
}

func (c *questionCard) leaveInput() {
	c.typing = false
}

func (c *questionCard) answersOf(q int) []string {
	answers := []string{}
	for j, option := range c.params.Questions[q].Options {
		if c.picked[q][j] {
			answers = append(answers, option.Label)
		}
	}
	if c.typed[q] != "" {
		answers = append(answers, c.typed[q])
	}
	return answers
}

func (c *questionCard) response() rpc.ToolRequestUserInputResponse {
	answers := make(map[string]rpc.ToolRequestUserInputAnswer, len(c.params.Questions))
	for i, q := range c.params.Questions {
		answers[q.ID] = rpc.ToolRequestUserInputAnswer{Answers: c.answersOf(i)}
	}
	return rpc.ToolRequestUserInputResponse{Answers: answers}
}

func cancelledResponse() rpc.ToolRequestUserInputResponse {
	return rpc.ToolRequestUserInputResponse{Answers: map[string]rpc.ToolRequestUserInputAnswer{}}
}

func (c *questionCard) answerLines(dismissed bool) []string {
	lines := make([]string, 0, len(c.params.Questions))
	for i, q := range c.params.Questions {
		switch answers := c.answersOf(i); {
		case dismissed:
			lines = append(lines, q.Header+": dismissed")
		case len(answers) == 0:
			lines = append(lines, q.Header+": skipped")
		default:
			lines = append(lines, q.Header+": "+strings.Join(answers, "; "))
		}
	}
	return lines
}

func (c *questionCard) view(s *screenState, hue, rows, width int) []string {
	if rows <= 0 {
		return nil
	}
	q := c.index
	question := c.params.Questions[q]
	title := s.fit("┌ "+fmt.Sprintf("Question %d of %d · %s", q+1, len(c.params.Questions), question.Header), width)
	if rows == 1 {
		return []string{title}
	}
	footer := c.hintLines(s, hue, width, max(rows-2, 1))
	if rows == 2 {
		return []string{title, footer[0]}
	}
	avail := rows - 1 - len(footer)
	questionLines := rail(s.wrap(question.Question, max(width-2, 1)), "│ ", width, s)
	options := c.optionLines(s, width)
	preview := c.previewLines(s, width)
	var body []string
	switch {
	case len(questionLines)+len(options)+len(preview) <= avail:
		body = slices.Concat(questionLines, options, preview)
	case len(questionLines)+len(options) <= avail:
		body = slices.Concat(questionLines, options)
	case len(options) <= avail:
		body = options
	default:
		body = scrollOptions(options, c.cursor[q], avail, s, width)
	}
	lines := []string{title}
	lines = append(lines, body...)
	for len(lines) < rows-len(footer) {
		lines = append(lines, s.fit("│", width))
	}
	return append(lines, footer...)
}

const (
	hintMove = iota
	hintPick
	hintToggle
	hintChoose
	hintNext
	questionHintCount
)

const (
	questionSegments   = questionHintCount + 1
	questionHintRail   = "└─ "
	questionHintIndent = "   "
	questionHintJoin   = " · "
	questionSubmit     = "s submit"
	questionActions    = questionSubmit + questionHintJoin + "Esc dismiss"
)

func (c *questionCard) hintPriority() [questionHintCount]int {
	if c.multi() {
		return [questionHintCount]int{hintToggle, hintMove, hintChoose, hintPick, hintNext}
	}
	return [questionHintCount]int{hintMove, hintChoose, hintPick, hintNext, hintToggle}
}

func hintBreaks(widths [questionSegments]int, shown, join, room int) (starts, lines int) {
	used := 0
	lines = 1
	for i, width := range widths {
		if shown&(1<<i) == 0 {
			continue
		}
		if used > 0 && used+join+width > room {
			starts |= 1 << i
			lines++
			used = 0
		}
		if used > 0 {
			used += join
		}
		used += width
	}
	return starts, lines
}

func (c *questionCard) hintShown(widths [questionSegments]int, join, room, maxLines int) int {
	shown := 1 << questionHintCount
	for _, i := range c.hintPriority() {
		if _, lines := hintBreaks(widths, shown|1<<i, join, room); lines <= maxLines {
			shown |= 1 << i
		}
	}
	return shown
}

func (c *questionCard) hintLines(s *screenState, hue, width, maxLines int) []string {
	texts := [questionSegments]string{
		hintMove:          "↑/↓ move",
		hintPick:          "1-7 pick",
		hintToggle:        "Space toggle",
		hintChoose:        "Enter choose",
		hintNext:          "Tab next",
		questionHintCount: s.dockTheme().liveTag(hue, questionSubmit) + questionHintJoin + "Esc dismiss",
	}
	var widths [questionSegments]int
	for i := range questionHintCount {
		widths[i] = s.cells(texts[i])
	}
	widths[questionHintCount] = s.cells(questionActions)
	join := s.cells(questionHintJoin)
	room := width - s.cells(questionHintRail)
	shown := c.hintShown(widths, join, room, maxLines)
	starts, _ := hintBreaks(widths, shown, join, room)
	return renderHints(s, texts, shown, starts, width)
}

func renderHints(s *screenState, texts [questionSegments]string, shown, starts, width int) []string {
	out := make([]string, 0, bits.OnesCount(uint(starts))+1)
	var line strings.Builder
	for i, text := range texts {
		if shown&(1<<i) == 0 {
			continue
		}
		if starts&(1<<i) != 0 {
			out = append(out, s.fit(line.String(), width))
			line.Reset()
		}
		switch {
		case line.Len() > 0:
			line.WriteString(questionHintJoin)
		case len(out) == 0:
			line.WriteString(questionHintRail)
		default:
			line.WriteString(questionHintIndent)
		}
		line.WriteString(text)
	}
	return append(out, s.fit(line.String(), width))
}

func rail(lines []string, prefix string, width int, s *screenState) []string {
	out := make([]string, 0, len(lines))
	for _, line := range lines {
		out = append(out, s.fit(prefix+line, width))
	}
	return out
}

func (c *questionCard) optionLine(s *screenState, width, j int) string {
	q := c.index
	question := c.params.Questions[q]
	picked := c.picked[q][j]
	mark := "( )"
	if picked {
		mark = "(•)"
	}
	if question.MultiSelect {
		mark = "[ ]"
		if picked {
			mark = "[x]"
		}
	}
	text := fmt.Sprintf("%d. %s", j+1, question.Options[j].Label)
	if j == 0 {
		text += "  Recommended"
	}
	prefix := "│ "
	if c.cursor[q] == j {
		prefix = "┃ › "
	}
	return s.fit(prefix+mark+" "+text, width)
}

func (c *questionCard) optionLines(s *screenState, width int) []string {
	q := c.index
	question := c.params.Questions[q]
	lines := make([]string, 0, c.rows(q))
	for j := range question.Options {
		lines = append(lines, c.optionLine(s, width, j))
	}
	if question.IsOther {
		prefix := "│ "
		if c.cursor[q] == len(question.Options) {
			prefix = "┃ › "
		}
		text := fmt.Sprintf("%d. Other", len(question.Options)+1)
		if c.typed[q] != "" {
			text += ": " + c.typed[q]
		}
		lines = append(lines, s.fit(prefix+text, width))
	}
	return lines
}

func (c *questionCard) previewLines(s *screenState, width int) []string {
	q := c.index
	options := c.params.Questions[q].Options
	description, preview := "", ""
	if row := c.cursor[q]; row < len(options) {
		description = options[row].Description
		first, _, _ := strings.Cut(options[row].Preview, "\n")
		preview = first
	}
	return []string{s.fit("│ "+description, width), s.fit("│ "+cutRunes(preview, width-4), width)}
}

func cutRunes(text string, n int) string {
	if n <= 0 || utf8.RuneCountInString(text) <= n {
		return text
	}
	return string([]rune(text)[:n-1]) + "…"
}

func scrollOptions(options []string, cursor, avail int, s *screenState, width int) []string {
	if avail <= 1 {
		return []string{options[min(cursor, len(options)-1)]}
	}
	start := min(max(cursor-avail/2, 0), len(options)-avail)
	body := slices.Clone(options[start : start+avail])
	row := cursor - start
	if start > 0 && row != 0 {
		body[0] = s.fit("│ ↑ more", width)
	}
	if start+avail < len(options) && row != avail-1 {
		body[avail-1] = s.fit("│ ↓ more", width)
	}
	return body
}

func decodeQuestion(params jsontext.Value) (rpc.ToolRequestUserInputParams, error) {
	request, err := decodeParams[rpc.ToolRequestUserInputParams](params)
	if err != nil {
		return request, fmt.Errorf("decode question: %w", err)
	}
	if len(request.Questions) == 0 {
		return request, errors.New("decode question: no questions")
	}
	for i, question := range request.Questions {
		if question.ID == "" {
			return request, fmt.Errorf("decode question: question %d has no id", i+1)
		}
	}
	return request, nil
}

func (m *model) applyQuestion(msg serverRequestMsg) tea.Cmd {
	params, err := decodeQuestion(msg.params)
	if err != nil {
		msg.answer <- requestAnswer{err: err}
		m.errorf("question request failed: %v; reconnect and retry the turn", err)
		return nil
	}
	card := newQuestionCard(params, msg.answer)
	m.input.field = textField{}
	m.approvals = append(m.approvals, approval{method: msg.method, requestID: msg.requestID, threadID: card.threadID, itemID: params.ItemID, card: card})
	if !m.acceptThread(card.threadID) {
		return nil
	}
	m.setState(stateAwaitingInput)
	return m.notify(m.notifyLine("Waiting for your answer"))
}

func questionResolvedNotice(resolution string) string {
	switch resolution {
	case "answered":
		return "Question answered in another window"
	case "cancelled":
		return "Question dismissed in another window"
	case "interrupted":
		return "Question closed: the turn was interrupted"
	}
	return "Question closed: the agent continues without an answer"
}

func (m *model) questionLines(width, rows int) []string {
	return m.approvals[0].card.view(&m.screen, threadHue(m.thread.id), rows, width)
}

func (m *model) typingAnswer() bool {
	return len(m.approvals) > 0 && m.approvals[0].card != nil && m.approvals[0].card.typing
}

func (m *model) answerQuestionKey(msg tea.KeyPressMsg) tea.Cmd {
	card := m.approvals[0].card
	if card.typing {
		return m.typeQuestionKey(card, msg)
	}
	wasTyping := card.typing
	action := card.key(msg)
	if !wasTyping && card.typing {
		m.input.field = typedField(card.typed[card.index])
	}
	m.applyCardAction(action)
	return nil
}

func (m *model) typeQuestionKey(card *questionCard, msg tea.KeyPressMsg) tea.Cmd {
	switch msg.String() {
	case "enter":
		text := m.input.field.value()
		m.input.field = textField{}
		action := card.commitText(text)
		if action == cardStay && card.typing {
			m.input.field = typedField(card.typed[card.index])
		}
		m.applyCardAction(action)
	case "esc":
		if card.freeText() {
			m.applyCardAction(cardDismiss)
			return nil
		}
		card.leaveInput()
		m.input.field = textField{}
	default:
		if !m.input.field.edit(msg.String()) && msg.Text != "" {
			m.input.field.insert(msg.Text)
		}
	}
	return nil
}

func typedField(text string) textField {
	runes := []rune(text)
	return textField{text: runes, cursor: len(runes)}
}

func (m *model) applyCardAction(action cardAction) {
	switch action {
	case cardStay:
	case cardSubmit:
		m.finishQuestion(false)
	case cardDismiss:
		m.finishQuestion(true)
	}
}

func (m *model) finishQuestion(dismissed bool) {
	card := m.approvals[0].card
	value := card.response()
	if dismissed {
		value = cancelledResponse()
	}
	card.answer <- requestAnswer{value: value}
	m.commit(fmt.Sprintf("question:%s:%s", card.threadID, card.params.ItemID), card.answerLines(dismissed))
	m.dropApproval(0)
}
