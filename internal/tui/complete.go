package tui

import (
	"cmp"
	"errors"
	"slices"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
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

func splitToken(text string) (token, tail string) {
	end := strings.IndexFunc(text, unicode.IsSpace)
	if end < 0 {
		end = len(text)
	}
	return text[:end], text[end:]
}

func (m *model) commandQuery() (string, bool) {
	text := m.input.composer.Value()
	if m.panelOpen() || m.review != nil || m.textMode() != inputPrompt || !strings.HasPrefix(text, "/") || strings.HasPrefix(text, "//") {
		return "", false
	}
	token, _ := splitToken(text)
	if m.input.composer.cursor > utf8.RuneCountInString(token) {
		return "", false
	}
	return strings.TrimPrefix(token, "/"), true
}

func (c *editor) tokenAt() (start, end int) {
	start, end = c.cursor, c.cursor
	for start > 0 && !unicode.IsSpace(c.text[start-1]) {
		start--
	}
	for end < len(c.text) && !unicode.IsSpace(c.text[end]) {
		end++
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
	return tea.Tick(fileDebounce, func(time.Time) tea.Msg { return debounceMsg{seq: seq} })
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
	params := fileSearchParams{Query: query, Roots: []string{m.conn.opts.Cwd}}
	return func() tea.Msg {
		if client == nil {
			return searchResultMsg{seq: seq, err: errors.New("rpc client is not connected")}
		}
		ctx, cancel := requestContext(done)
		defer cancel()
		var reply fileSearchReply
		err := client.Call(ctx, "fuzzyFileSearch", params, &reply)
		return searchResultMsg{seq: seq, files: reply.Files, err: err}
	}
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
		if len(popup.items) == 0 {
			return nil, popup.kind == popupFile
		}
		return m.runPopup(), true
	}
	return nil, false
}

func (m *model) completePopup() {
	if m.input.popup.kind == popupFile {
		m.completeFile()
		return
	}
	name := m.input.popup.selected()
	m.input.popup = nil
	_, tail := splitToken(m.input.composer.Value())
	tail = cmp.Or(tail, " ")
	head := "/" + name
	m.input.composer.SetValue(head + tail)
	m.input.composer.cursor = utf8.RuneCountInString(head + " ")
}

func (m *model) completeFile() {
	popup := m.input.popup
	m.input.popup = nil
	e := &m.input.composer
	start, end := e.tokenAt()
	insert := []rune("@" + popup.selected() + " ")
	e.text = slices.Concat(e.text[:start], insert, e.text[end:])
	e.cursor = start + len(insert)
}

func (m *model) runPopup() tea.Cmd {
	if m.input.popup.kind == popupFile {
		m.completeFile()
		return nil
	}
	name := m.input.popup.selected()
	_, tail := splitToken(m.input.composer.Value())
	if commandNamed(name).args != "" || tail != "" {
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
	width := terminalWidth(m.screen.width)
	lines := make([]string, 0, rows)
	for i := range rows {
		if i >= len(popup.items) {
			line := ""
			if i == 0 && popup.notice != "" {
				line = m.screen.fit(popup.notice, width)
			}
			lines = append(lines, line)
			continue
		}
		item := popup.items[i]
		prefix := "  "
		if i == popup.cursor {
			prefix = "┃ "
		}
		line := m.screen.fit(prefix+item.label+"  "+item.detail, width)
		if i == popup.cursor {
			line = m.screen.dockTheme().paintBold(roleText, line)
		}
		lines = append(lines, line)
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
