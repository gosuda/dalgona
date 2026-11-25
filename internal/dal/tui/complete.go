package tui

import (
	"cmp"
	"errors"
	"slices"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/textinput"
)

const (
	popupItems      = 8
	popupCommand    = "command"
	popupFile       = "file"
	fileDebounce    = 120 * time.Millisecond
	fileSearchGone  = "File search is unavailable on this server"
	fileRPCNotFound = -32601
)

type completion struct {
	kind   string
	items  []listItem
	cursor int
	query  string
	seq    int
	notice string
}

type fileHit struct {
	Path string `json:"path"`
}

type fileSearchParams struct {
	Query string   `json:"query"`
	Roots []string `json:"roots"`
}

type fileSearchReply struct {
	Files []fileHit `json:"files"`
}

func newCompletion(query string) *completion {
	return &completion{kind: popupCommand, items: commandItems(query)}
}

func newFileCompletion(query string, seq int) *completion {
	return &completion{kind: popupFile, query: query, seq: seq}
}

func (c *completion) refilter(query string) {
	c.items = commandItems(query)
	c.cursor = min(c.cursor, max(0, len(c.items)-1))
}

func (c *completion) move(delta int) {
	c.cursor = min(max(c.cursor+delta, 0), max(0, len(c.items)-1))
}

func (c *completion) selected() string {
	return c.items[c.cursor].id
}

func commandItems(query string) []listItem {
	type ranked struct {
		item  listItem
		score int
	}
	var found []ranked
	for _, c := range commands {
		if score := fuzzyScore(query, c.name); score > 0 {
			found = append(found, ranked{item: listItem{id: c.name, label: commandLabel(c), detail: c.summary}, score: score})
		}
	}
	slices.SortStableFunc(found, func(a, b ranked) int { return cmp.Compare(b.score, a.score) })
	found = found[:min(len(found), popupItems)]
	items := make([]listItem, 0, len(found))
	for _, r := range found {
		items = append(items, r.item)
	}
	return items
}

func commandLabel(c command) string {
	if c.args == "" {
		return "/" + c.name
	}
	return "/" + c.name + " " + c.args
}

func commandNamed(name string) command {
	return commands[slices.IndexFunc(commands, func(c command) bool { return c.name == name })]
}

func (c editor) slashEnd(start int) int {
	for i, r := range c.text[start:] {
		if r == ' ' || c.hasChipsAt(start+i) {
			return start + i
		}
	}
	return len(c.text)
}

func (m *model) commandQuery() (string, bool) {
	e := &m.input.composer
	start := e.slashStart()
	if m.panelOpen() || m.review != nil || m.textMode() != inputPrompt || !e.readsSlash() {
		return "", false
	}
	end := e.slashEnd(start)
	if e.cursor < start || e.cursor > end || e.lead > 0 {
		return "", false
	}
	query := string(e.text[start+1 : end])
	return query, !strings.HasPrefix(query, "/")
}

func (c editor) tokenAt() (start, end int) {
	start, end = c.cursor, c.cursor
	blocked := c.lead > 0
	for start > 0 && !blocked && !textinput.IsWhiteSpace(c.text[start-1]) {
		start--
		blocked = c.hasChipsAt(start)
	}
	blocked = c.lead < c.chipsAt(c.cursor)
	for end < len(c.text) && !blocked && !textinput.IsWhiteSpace(c.text[end]) {
		end++
		blocked = c.hasChipsAt(end)
	}
	return start, end
}

func (m *model) fileQuery() (string, bool) {
	if m.panelOpen() || m.review != nil || m.textMode() != inputPrompt {
		return "", false
	}
	start, end := m.input.composer.tokenAt()
	if start == end || m.input.composer.text[start] != '@' {
		return "", false
	}
	return string(m.input.composer.text[start+1 : end]), true
}

func (m *model) syncPopup(typed bool) tea.Cmd {
	if query, ok := m.commandQuery(); ok {
		switch {
		case m.input.popup != nil && m.input.popup.kind == popupCommand:
			m.input.popup.refilter(query)
		case typed:
			m.input.popup = newCompletion(query)
		default:
			m.input.popup = nil
		}
		return nil
	}
	if query, ok := m.fileQuery(); ok {
		if typed {
			return m.openFilePopup(query)
		}
		return m.refreshFilePopup(query)
	}
	m.input.popup = nil
	return nil
}

func (m *model) openFilePopup(query string) tea.Cmd {
	m.input.fileSeq++
	m.input.popup = newFileCompletion(query, m.input.fileSeq)
	return m.debounceFile(m.input.fileSeq)
}

func (m *model) refreshFilePopup(query string) tea.Cmd {
	popup := m.input.popup
	if popup == nil || popup.kind != popupFile {
		m.input.popup = nil
		return nil
	}
	if popup.query == query {
		return nil
	}
	m.input.fileSeq++
	popup.query, popup.seq = query, m.input.fileSeq
	popup.items, popup.cursor = nil, 0
	return m.debounceFile(popup.seq)
}

func (m *model) debounceFile(seq int) tea.Cmd {
	return m.conn.link.scope(tea.Tick(fileDebounce, func(time.Time) tea.Msg { return debounceMsg{seq: seq} }))
}

func (m *model) applyDebounce(msg debounceMsg) tea.Cmd {
	popup := m.input.popup
	if popup == nil || popup.kind != popupFile || popup.seq != msg.seq || popup.query == "" {
		return nil
	}
	return m.searchFiles(msg.seq, popup.query)
}

func (m *model) searchFiles(seq int, query string) tea.Cmd {
	client, done := m.conn.client, m.conn.done
	params := fileSearchParams{Query: query, Roots: []string{m.workDir()}}
	return m.conn.link.scope(func() tea.Msg {
		if client == nil {
			return searchResultMsg{seq: seq, err: errors.New("rpc client is not connected")}
		}
		ctx, cancel := requestContext(done)
		defer cancel()
		var reply fileSearchReply
		err := client.Call(ctx, "fuzzyFileSearch", params, &reply)
		return searchResultMsg{seq: seq, files: reply.Files, err: err}
	})
}

func (m *model) applySearch(msg searchResultMsg) {
	popup := m.input.popup
	if popup == nil || popup.kind != popupFile || popup.seq != msg.seq {
		return
	}
	popup.items = nil
	if msg.err != nil {
		m.fileSearchFailed(popup, msg.err)
		return
	}
	for _, hit := range msg.files[:min(len(msg.files), popupItems)] {
		path := sanitizeText(hit.Path)
		popup.items = append(popup.items, listItem{id: path, label: path})
	}
	popup.cursor = min(popup.cursor, max(0, len(popup.items)-1))
}

func (m *model) fileSearchFailed(popup *completion, err error) {
	rpcErr, ok := errors.AsType[*rpc.RPCError](err)
	if !ok || rpcErr.Code != fileRPCNotFound || m.input.fileMissing {
		return
	}
	m.input.fileMissing = true
	popup.notice = fileSearchGone
}

func (m *model) popupShown() bool {
	return m.input.popup != nil && !m.panelOpen() && m.review == nil
}

func (m *model) popupKey(msg tea.KeyPressMsg) (tea.Cmd, bool) {
	if !m.popupShown() {
		return nil, false
	}
	popup := m.input.popup
	switch msg.String() {
	case "esc":
		m.input.popup = nil
		return nil, true
	case "up", "down":
		if len(popup.items) == 0 {
			return nil, false
		}
		popup.move(map[string]int{"up": -1, "down": 1}[msg.String()])
		return nil, true
	case "tab":
		if len(popup.items) == 0 {
			return nil, false
		}
		m.completePopup()
		return nil, true
	case "enter":
		return m.popupEnter(popup)
	}
	return nil, false
}

func (m *model) popupEnter(popup *completion) (tea.Cmd, bool) {
	if len(popup.items) == 0 {
		return nil, popup.kind == popupFile
	}
	return m.runPopup(), true
}

func (m *model) completePopup() {
	if m.input.popup.kind == popupFile {
		m.completeFile()
		return
	}
	name := m.input.popup.selected()
	m.input.popup = nil
	e := &m.input.composer
	end := e.slashEnd(e.slashStart())
	head := []rune("/" + name + " ")
	tail := e.text[end:]
	if len(tail) > 0 && tail[0] == ' ' && !e.hasChipsAt(end) {
		tail = tail[1:]
	}
	shift := len(head) + len(tail) - len(e.text)
	for i := range e.chips {
		e.chips[i].at = max(e.chips[i].at+shift, len(head))
	}

	e.text = slices.Concat(head, tail)
	e.cursor, e.lead = len(head), 0
	e.verbatimText = nil
	e.repairCaret()
}

func (m *model) completeFile() {
	popup := m.input.popup
	m.input.popup = nil
	e := &m.input.composer
	start, end := e.tokenAt()
	insert := []rune("@" + popup.selected() + " ")
	first := e.chipIndex(end)
	for i := range e.chips[first:] {
		e.chips[first+i].at += len(insert) - (end - start)
	}
	e.text = slices.Concat(e.text[:start], insert, e.text[end:])
	e.cursor, e.lead = start+len(insert), 0
	e.repairCaret()
}

func (m *model) runPopup() tea.Cmd {
	if m.input.popup.kind == popupFile {
		m.completeFile()
		return nil
	}
	name := m.input.popup.selected()
	e := &m.input.composer
	if commandNamed(name).args != "" || e.slashEnd(e.slashStart()) < len(e.text) || len(e.chips) > 0 {
		m.completePopup()
		return nil
	}
	m.input.popup = nil
	m.input.composer.SetValue("/" + name)
	return m.submitPrompt()
}

func (m *model) popupLines() []string {
	popup := m.input.popup
	rows := panelRows(panelPopup, m.screen.height)
	th := m.screen.dockTheme()
	width := panelRowWidth(&m.screen)
	lines := make([]string, 0, rows)
	for i := range rows {
		if i >= len(popup.items) {
			line := ""
			if i == 0 && popup.notice != "" {
				line = th.paint(roleDim, m.screen.fit(popup.notice, max(1, width-panelRailCells-2)))
			}
			lines = append(lines, m.screen.fit(th.paint(roleFaint, "│ ")+"  "+line, width))
			continue
		}
		item := popup.items[i]
		selected := i == popup.cursor
		rail := th.paint(roleFaint, "│ ")
		marker := "  "
		label := th.paint(roleText, item.label)
		if selected {
			rail = th.paintBold(roleText, "┃ ")
			marker = th.paintBold(roleAccentText, "● ")
			label = th.paintBold(roleText, item.label)
		}
		detail := ""
		if item.detail != "" {
			detail = "  " + th.paint(roleDim, item.detail)
		}
		lines = append(lines, m.screen.fit(rail+marker+label+detail, width))
	}
	return lines
}

func typedText(msg tea.Msg) bool {
	switch msg := msg.(type) {
	case tea.KeyPressMsg:
		return msg.Text != ""
	case tea.PasteMsg:
		return msg.Content != ""
	}
	return false
}
