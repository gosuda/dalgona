package tui

import (
	"cmp"
	"encoding/json/jsontext"
	"fmt"
	"math/bits"
	"slices"
	"strconv"
	"strings"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/textinput"
)

const stateAwaitingInput = "awaiting input"

type cardAction int

const (
	cardStay cardAction = iota
	cardSubmit
	cardDismiss
)

type questionCard struct {
	threadID  string
	requestID string
	params    rpc.ToolRequestUserInputParams
	text      []questionText
	answer    chan requestAnswer
	index     int
	cursor    []int
	picked    [][]bool
	typed     []string
	typing    bool
	sent      bool
	attempts  int
	reviewTop int
}

type questionText struct {
	header, question               string
	labels, descriptions, previews []string
}

func newQuestionCard(params rpc.ToolRequestUserInputParams, requestID string, answer chan requestAnswer) *questionCard {
	card := &questionCard{threadID: params.ThreadID, requestID: requestID, params: params, answer: answer}
	card.text = make([]questionText, len(params.Questions))
	card.cursor = make([]int, len(params.Questions))
	card.typed = make([]string, len(params.Questions))
	card.picked = make([][]bool, len(params.Questions))
	for i, q := range params.Questions {
		card.picked[i] = make([]bool, len(q.Options))
		card.text[i] = projectQuestion(q)
	}
	card.typing = card.freeText()
	return card
}

func projectQuestion(q rpc.ToolRequestUserInputQuestion) questionText {
	shown := questionText{header: textinput.DisplayLine(q.Header), question: textinput.DisplayLine(q.Question)}
	labels := make([]string, len(q.Options))
	for j, o := range q.Options {
		labels[j] = o.Label
		shown.descriptions = append(shown.descriptions, textinput.DisplayLine(o.Description))
		shown.previews = append(shown.previews, textinput.DisplayBlock(o.Preview))
	}
	shown.labels = textinput.DistinctLines(labels)
	return shown
}

func (c *questionCard) isReview() bool {
	return c.index == len(c.params.Questions)
}

func (c *questionCard) tabCount() int {
	return len(c.params.Questions) + 1
}

func (c *questionCard) freeText() bool {
	return !c.isReview() && len(c.params.Questions[c.index].Options) == 0
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
	if len(c.params.Questions) == 0 {
		return cardDismiss
	}
	if c.navigate(key) {
		return cardStay
	}
	if c.isReview() {
		return reviewChoose(key)
	}
	return c.choose(key)
}

func (c *questionCard) navigate(key string) bool {
	switch key {
	case "up", "k":
		c.moveCursor(-1)
	case "down", "j":
		c.moveCursor(1)
	case "tab", "right", "l":
		c.moveQuestion(1)
	case "shift+tab", "left", "h":
		c.moveQuestion(-1)
	default:
		return false
	}
	return true
}

func (c *questionCard) moveCursor(delta int) {
	if c.isReview() {
		return
	}
	last := max(c.rows(c.index)-1, 0)
	c.cursor[c.index] = min(max(c.cursor[c.index]+delta, 0), last)
}

func (c *questionCard) moveQuestion(delta int) {
	tabs := c.tabCount()
	c.index = (c.index + delta + tabs) % tabs
	c.reviewTop = 0
	c.typing = c.freeText()
}

func reviewChoose(key string) cardAction {
	switch key {
	case "s", "enter":
		return cardSubmit
	case "esc":
		return cardDismiss
	}
	return cardStay
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
	return !c.isReview() && c.params.Questions[c.index].MultiSelect
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
	c.moveQuestion(1)
	return cardStay
}

func (c *questionCard) commitText(text string) cardAction {
	q := c.index
	c.typed[q] = text
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

func (c *questionCard) shownAnswers(q int) []string {
	var shown []string
	for j := range c.params.Questions[q].Options {
		if c.picked[q][j] {
			shown = append(shown, c.text[q].labels[j])
		}
	}
	if typed := c.typed[q]; typed != "" {
		shown = append(shown, cmp.Or(textinput.DisplayLine(typed), textinput.Exposed(typed)))
	}
	return shown
}

func (c *questionCard) checkAnswers() error {
	field := textinput.Field{Name: "answer", MaxScalars: textinput.AnswerScalars}
	for i := range c.params.Questions {
		for _, answer := range c.answersOf(i) {
			if err := field.Check(answer); err != nil {
				return err
			}
		}
	}
	return nil
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

func (c *questionCard) entryKey(kind string) string {
	fields := []string{kind, c.threadID, c.params.TurnID, c.params.ItemID, c.requestID}
	for i := 1; i < len(fields); i++ {
		fields[i] = strconv.Quote(fields[i])
	}
	return strings.Join(fields, ":")
}

func (c *questionCard) submissionLines(dismissed bool) []string {
	lines := make([]string, 0, len(c.params.Questions))
	for i := range c.params.Questions {
		entry := "dismiss requested"
		if !dismissed {
			entry = cmp.Or(strings.Join(c.shownAnswers(i), "; "), "skip requested")
		}
		lines = append(lines, fmt.Sprintf("%s: Local submission #%d: %s", c.text[i].header, c.attempts, entry))
	}
	return lines
}

func (c *questionCard) statusLines(status string) []string {
	lines := make([]string, 0, len(c.params.Questions))
	for i := range c.params.Questions {
		lines = append(lines, c.text[i].header+": "+status)
	}
	return lines
}

func (c *questionCard) reopen(field string) {
	c.sent = false
	path, ok := strings.CutPrefix(field, "answers.")
	cut := strings.LastIndex(path, ".answers[")
	if !ok || cut < 0 {
		return
	}
	q := slices.IndexFunc(c.params.Questions, func(q rpc.ToolRequestUserInputQuestion) bool { return q.ID == path[:cut] })
	if q < 0 {
		return
	}
	c.index, c.reviewTop = q, 0
	if c.typed[q] == "" {
		c.typing = c.freeText()
		return
	}
	c.cursor[q] = len(c.params.Questions[q].Options)
	c.typing = true
}

func (c *questionCard) view(s *screenState, hue, rows, width int) []string {
	if rows <= 0 {
		return nil
	}
	if c.isReview() {
		return c.reviewView(s, hue, rows, width)
	}
	q := c.index
	title := s.fit("┌ "+fmt.Sprintf("Question %d of %d · %s", q+1, len(c.params.Questions), c.text[q].header), width)
	if rows == 1 {
		return []string{title}
	}
	footer := c.hintLines(s, hue, width, max(rows-2, 1))
	if rows == 2 {
		return []string{title, footer[0]}
	}
	avail := rows - 1 - len(footer)
	questionLines := rail(s.wrap(c.text[q].question, max(width-2, 1)), "│ ", width, s)
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
	return s.padCard(append([]string{title}, body...), footer, rows, width)
}

func (s *screenState) padCard(lines, footer []string, rows, width int) []string {
	for len(lines) < rows-len(footer) {
		lines = append(lines, s.fit("│", width))
	}
	return append(lines, footer...)
}

func (c *questionCard) reviewTitle() string {
	if len(c.params.Questions) == 1 {
		return "Review · 1 question"
	}
	return fmt.Sprintf("Review · %d questions", len(c.params.Questions))
}

func (c *questionCard) reviewBody(s *screenState, width int) []string {
	t := s.dockTheme()
	lines := make([]string, 0, 2*len(c.params.Questions))
	for i := range c.params.Questions {
		lines = append(lines, s.fit("│ "+t.paintBold(roleText, c.text[i].header), width))
		answer := t.paint(roleDim, "(no answer)")
		if answers := c.shownAnswers(i); len(answers) > 0 {
			answer = t.paint(roleText, strings.Join(answers, "; "))
		}
		lines = append(lines, s.fit("│   "+answer, width))
	}
	return lines
}

func (c *questionCard) reviewFrame(s *screenState, hue, rows, width int) (body, footer []string) {
	body = c.reviewBody(s, width)
	maxLines := max(rows-2, 1)
	switch {
	case c.sent:
		footer = c.sentFooter(s, width)
	default:
		footer = reviewHints(s, hue).lines(s, width, maxLines)
		if len(body) > rows-1-len(footer) {
			footer = reviewScrollHints(s, hue).lines(s, width, maxLines)
		}
	}
	return body, footer
}

func (c *questionCard) reviewView(s *screenState, hue, rows, width int) []string {
	title := s.fit("┌ "+c.reviewTitle(), width)
	if rows == 1 {
		return []string{title}
	}
	body, footer := c.reviewFrame(s, hue, rows, width)
	if rows == 2 {
		return []string{title, footer[0]}
	}
	avail := rows - 1 - len(footer)
	top := min(c.reviewTop, max(len(body)-avail, 0))
	return s.padCard(append([]string{title}, body[top:min(top+avail, len(body))]...), footer, rows, width)
}

func (c *questionCard) scrollReview(delta, total, avail int) {
	limit := max(total-avail, 0)
	c.reviewTop = min(max(min(c.reviewTop, limit)+delta, 0), limit)
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

type hintSet struct {
	texts    [questionSegments]string
	priority []int
}

func (h hintSet) shown(widths [questionSegments]int, join, room, maxLines int) int {
	shown := 1 << questionHintCount
	for _, i := range h.priority {
		if _, lines := hintBreaks(widths, shown|1<<i, join, room); lines <= maxLines {
			shown |= 1 << i
		}
	}
	return shown
}

func (h hintSet) lines(s *screenState, width, maxLines int) []string {
	var widths [questionSegments]int
	for i := range questionHintCount {
		widths[i] = s.cells(h.texts[i])
	}
	widths[questionHintCount] = s.cells(questionActions)
	join := s.cells(questionHintJoin)
	room := width - s.cells(questionHintRail)
	shown := h.shown(widths, join, room, maxLines)
	starts, _ := hintBreaks(widths, shown, join, room)
	return renderHints(s, h.texts, shown, starts, width)
}

func (c *questionCard) sentFooter(s *screenState, width int) []string {
	return []string{s.dockTheme().paint(roleDim, s.fit(questionHintRail+"Answer sent; waiting for the agent", width))}
}

func (c *questionCard) hintLines(s *screenState, hue, width, maxLines int) []string {
	if c.sent {
		return c.sentFooter(s, width)
	}
	priority := c.hintPriority()
	set := hintSet{priority: priority[:], texts: [questionSegments]string{
		hintMove:          "↑/↓ move",
		hintPick:          "1-7 pick",
		hintToggle:        "Space toggle",
		hintChoose:        "Enter choose",
		hintNext:          "Tab next",
		questionHintCount: s.dockTheme().liveTag(hue, questionSubmit) + questionHintJoin + "Esc dismiss",
	}}
	return set.lines(s, width, maxLines)
}

func reviewHints(s *screenState, hue int) hintSet {
	return hintSet{priority: []int{hintChoose, hintNext}, texts: [questionSegments]string{
		hintChoose:        "Enter submit",
		hintNext:          "Tab back",
		questionHintCount: s.dockTheme().liveTag(hue, questionSubmit) + questionHintJoin + "Esc dismiss",
	}}
}

func reviewScrollHints(s *screenState, hue int) hintSet {
	set := reviewHints(s, hue)
	set.texts[hintMove] = "↑/↓ scroll"
	set.priority = []int{hintChoose, hintNext, hintMove}
	return set
}

func renderHints(s *screenState, texts [questionSegments]string, shown, starts, width int) []string {
	out := make([]string, 0, bits.OnesCount(uint(starts))+1)
	var line strings.Builder
	for i, text := range texts {
		if shown&(1<<i) == 0 {
			continue
		}
		if starts&(1<<i) != 0 {
			out = append(out, s.dockTheme().paint(roleDim, s.fit(line.String(), width)))
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
	return append(out, s.dockTheme().paint(roleDim, s.fit(line.String(), width)))
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
	text := fmt.Sprintf("%d. %s", j+1, c.text[q].labels[j])
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
		if typed := c.typed[q]; typed != "" {
			text += ": " + cmp.Or(textinput.DisplayLine(typed), textinput.Exposed(typed))
		}
		lines = append(lines, s.fit(prefix+text, width))
	}
	return lines
}

func (c *questionCard) previewLines(s *screenState, width int) []string {
	q := c.index
	description, preview := "", ""
	if row := c.cursor[q]; row < len(c.text[q].labels) {
		description = c.text[q].descriptions[row]
		preview = c.text[q].previews[row]
	}
	first, _, _ := strings.Cut(preview, "\n")
	suffix := ""
	if more := strings.Count(preview, "\n"); more > 0 {
		suffix = fmt.Sprintf(" … %d more lines · ctrl+o", more)
	}
	room := max(width-utf8.RuneCountInString(suffix)-3, 1)
	return []string{s.fit("│ "+description, width), s.fit("│ "+cutRunes(first, room)+suffix, width)}
}

func (c *questionCard) reviewLines() []string {
	var lines []string
	for i, q := range c.params.Questions {
		shown := c.text[i]
		lines = append(lines, fmt.Sprintf("Question %d of %d · %s", i+1, len(c.params.Questions), shown.header))
		lines = append(lines, strings.Split(textinput.DisplayBlock(q.Question), "\n")...)
		for j, o := range q.Options {
			lines = append(lines, fmt.Sprintf("%d. %s", j+1, shown.labels[j]))
			if block := textinput.DisplayBlock(o.Description); block != "" {
				lines = append(lines, strings.Split(block, "\n")...)
			}
			if shown.previews[j] != "" {
				lines = append(lines, strings.Split(shown.previews[j], "\n")...)
			}
		}
		if q.IsOther {
			lines = append(lines, fmt.Sprintf("%d. Other", len(q.Options)+1))
		}
	}
	return lines
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
	if len(params.Questions) == 0 {
		msg.answer <- requestAnswer{value: cancelledResponse()}
		return nil
	}
	if open := m.openQuestion(params.ThreadID, msg.requestID); open != nil {
		open.answer, open.sent = msg.answer, false
		return nil
	}
	card := newQuestionCard(params, msg.requestID, msg.answer)
	if len(m.approvals) == 0 && (card.typing || !m.loginTyping()) {
		m.input.field = textField{}
	}
	m.approvals = append(m.approvals, approval{method: msg.method, requestID: msg.requestID, threadID: card.threadID, itemID: params.ItemID, card: card})
	if !m.acceptThread(card.threadID) {
		return nil
	}
	m.setState(stateAwaitingInput)
	return m.notify(m.notifyLine("Waiting for your answer"))
}

func (m *model) openQuestion(threadID, requestID string) *questionCard {
	for _, open := range m.approvals {
		if open.card != nil && open.threadID == threadID && open.requestID == requestID {
			return open.card
		}
	}
	return nil
}

func (m *model) applyAnswerWarning(params jsontext.Value) error {
	event, err := decodeParams[rpc.WarningNotification](params)
	if err != nil || event.RequestID == "" {
		return err
	}
	card := m.openQuestion(event.ThreadID, event.RequestID)
	if card == nil || !card.sent {
		return nil
	}
	card.reopen(event.Field)
	if m.approvals[0].card == card && card.typing {
		m.input.field = typedField(card.typed[card.index])
	}
	m.errorf("Answer not accepted: %s; edit it and submit again", event.Message)
	return nil
}

func questionResolvedNotice(resolution string, sent bool) string {
	if sent && resolution == "answered" {
		return "Question answered"
	}
	switch resolution {
	case "answered":
		return "Question answered in another window"
	case "cancelled":
		return "Question dismissed in another window"
	case "interrupted":
		return "Question closed: the turn was interrupted"
	case "timedOut":
		return "Question timed out"
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
	if card.sent {
		return nil
	}
	if card.typing {
		return m.typeQuestionKey(card, msg)
	}
	if delta := reviewScrollDelta(msg.String()); card.isReview() && delta != 0 {
		m.scrollQuestionReview(card, delta)
		return nil
	}
	wasTyping := card.typing
	action := card.key(msg)
	if !wasTyping && card.typing {
		m.input.field = typedField(card.typed[card.index])
	}
	m.applyCardAction(action)
	return nil
}

func reviewScrollDelta(key string) int {
	switch key {
	case "up", "k":
		return -1
	case "down", "j":
		return 1
	}
	return 0
}

func (m *model) scrollQuestionReview(card *questionCard, delta int) {
	rows := panelRows(panelAsk, m.screen.height)
	body, footer := card.reviewFrame(&m.screen, threadHue(m.thread.id), rows, terminalWidth(m.screen.width))
	card.scrollReview(delta, len(body), max(rows-1-len(footer), 0))
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
			m.typeIntoField(msg.Text)
		}
	}
	return nil
}

func (m *model) handOverField(dropped approval) {
	if m.typingAnswer() {
		card := m.approvals[0].card
		m.input.field = typedField(card.typed[card.index])
		return
	}
	if dropped.card != nil && dropped.card.typing {
		m.input.field = textField{}
	}
}

func typedField(text string) textField {
	runes := []rune(text)
	return textField{text: runes, cursor: len(runes), limit: textinput.AnswerScalars}
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
	value := cancelledResponse()
	if !dismissed {
		if err := card.checkAnswers(); err != nil {
			m.errorf("Answer not sent: %v", err)
			return
		}
		value = card.response()
	}
	select {
	case card.answer <- requestAnswer{value: value}:
	default:
		m.errorf("Answer not sent: the previous answer is still being sent")
		return
	}
	card.attempts++
	m.commit(card.entryKey("question-submission")+":"+strconv.Itoa(card.attempts), card.submissionLines(dismissed))
	if dismissed {
		m.dropApproval(0)
		return
	}
	if card.typing {
		m.input.field = textField{}
	}
	card.sent, card.typing = true, false
	m.noticef("Answer sent; waiting for the agent")
}
