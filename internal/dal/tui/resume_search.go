package tui

import (
	"cmp"
	"context"
	"errors"
	"io"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/textinput"
)

const resumeSearchDelay = 300 * time.Millisecond

type resumeSearch struct {
	query      string
	rows       []listItem
	cancel     context.CancelFunc
	nextCursor string
	err        string
}

type resumeSearchPage struct {
	search *resumeSearch
	page   rpc.ThreadSearchResponse
}

func (p *listPanel) stopResumeSearch() {
	if p.search != nil {
		if p.search.cancel != nil {
			p.search.cancel()
		}
		p.cursor, p.scroll = 0, 0
		p.search = nil
	}
}

func (m *model) syncResumeSearch() tea.Cmd {
	list := m.list
	if list == nil || list.kind != "resume" {
		return nil
	}
	query := m.resumeQuery()
	if list.search != nil && list.search.query == query {
		return nil
	}
	list.stopResumeSearch()
	if query == "" {
		return nil
	}
	list.cursor, list.scroll = 0, 0
	list.search = &resumeSearch{query: query, rows: list.filtered(query)}
	return m.fetchResumeMatches(list.search, resumeSearchDelay)
}

func (m *model) resumeQuery() string {
	if m.textMode() != inputFilter || m.conn.lost != "" || m.review != nil {
		return ""
	}
	query := m.input.field.value()
	if !textinput.SearchEligible(query) {
		return ""
	}
	return query
}

func (m *model) fetchResumeMatches(search *resumeSearch, delay time.Duration) tea.Cmd {
	ctx, cancel := context.WithCancel(context.Background())
	search.cancel = cancel
	search.err = ""
	params := rpc.ThreadSearchParams{Query: search.query, Cursor: search.nextCursor, Limit: 50}
	conn := m.conn
	return conn.link.scope(func() tea.Msg {
		defer cancel()
		if delay > 0 {
			select {
			case <-time.After(delay):
			case <-ctx.Done():
			}
		}
		reply := &resumeSearchPage{search: search}
		err := conn.searchResumePage(ctx, params, &reply.page)
		return resultMsg{op: "resumeSearch", value: reply, err: err}
	})
}

func (c connInfo) searchResumePage(ctx context.Context, params rpc.ThreadSearchParams, page *rpc.ThreadSearchResponse) error {
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-c.done:
		return io.ErrClosedPipe
	case c.resumeSlot <- struct{}{}:
	}
	defer func() { <-c.resumeSlot }()
	if err := ctx.Err(); err != nil {
		return err
	}
	if c.client == nil {
		return errors.New("rpc client is not connected")
	}
	callCtx, cancel := requestContext(c.done)
	defer cancel()
	return c.client.Call(callCtx, "dalgona/thread/search", params, page)
}

func (m *model) applyResumeSearch(msg resultMsg) {
	reply, ok := msg.value.(*resumeSearchPage)
	if !ok || m.list == nil || m.list.search != reply.search {
		return
	}
	search := reply.search
	search.cancel = nil
	if msg.err != nil {
		search.err = resumeText(msg.err.Error())
		return
	}
	m.list.appendResumeMatches(reply.page, m.thread.id)
}

func (p *listPanel) appendResumeMatches(page rpc.ThreadSearchResponse, activeID string) {
	search := p.search
	index := make(map[string]int, len(search.rows))
	for i, row := range search.rows {
		index[row.id] = i
	}
	recent := make(map[string]listItem, len(p.items))
	for _, row := range p.items {
		recent[row.id] = row
	}
	for _, hit := range page.Data {
		snippet := resumeText(hit.Snippet)
		if i, ok := index[hit.ThreadID]; ok {
			search.rows[i].snippet = snippet
			continue
		}
		row, ok := recent[hit.ThreadID]
		if !ok {
			row = listItem{
				id:     hit.ThreadID,
				label:  resumeText(cmp.Or(hit.Name, hit.Preview, hit.ThreadID)),
				date:   threadDate(rpc.Thread{UpdatedAt: hit.UpdatedAt}, p.openedAt),
				active: hit.ThreadID == activeID,
			}
		}
		row.snippet = snippet
		index[hit.ThreadID] = len(search.rows)
		search.rows = append(search.rows, row)
	}
	search.nextCursor = page.NextCursor
}

func resumeText(text string) string {
	return strings.Join(strings.Fields(sanitizeText(text)), " ")
}

func (p *listPanel) resumeHint() string {
	switch search := p.search; {
	case search == nil:
		return "Search with 3+ characters per word · " + listHint
	case search.cancel != nil:
		return "Searching conversations… · Enter open · Esc cancel"
	case search.err != "":
		return "Tab retry · Search failed: " + search.err
	case search.nextCursor != "":
		return "Tab more matches · ↑/↓ choose · Enter open"
	case len(search.rows) == 0:
		return "No conversation mentions this text. Esc clears the filter."
	default:
		return listHint
	}
}

func (m *model) resumePreview() []string {
	search := m.list.search
	i := m.list.cursor
	if search == nil || i < 0 || i >= len(search.rows) || search.rows[i].snippet == "" {
		return nil
	}
	lines := m.screen.wrap(search.rows[i].snippet, max(1, panelRowWidth(&m.screen)-4))
	room := max(0, panelRows(panelList, m.screen.height)-3)
	return lines[:min(len(lines), room)]
}
