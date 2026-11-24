package state

import (
	"encoding/json/jsontext"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

const (
	methodThreadList = "thread/list"
	lanePageLimit    = 100
)

type (
	LoadThreads struct{ Endpoint client.EndpointID }
	LaneView    struct {
		Rows    []rpc.Thread
		More    bool
		Loading bool
		Loaded  bool
		Err     error
	}
	laneState struct {
		rows     []rpc.Thread
		archived map[string]rpc.Thread
		cursor   string
		more     bool
		loaded   bool
		pages    int
		ticket   uint64
		request  string
		err      error
		check    laneCheck
	}
	laneCheck struct {
		active bool
		at     int
		left   int
	}
	laneCall struct {
		link   client.Link
		cursor string
		check  bool
	}
)

var laneUpdates = map[string]func(*laneState, jsontext.Value) bool{
	"thread/started": func(l *laneState, raw jsontext.Value) bool {
		note, err := decode[rpc.ThreadStartedNotification](raw)
		return err == nil && l.insert(note.Thread)
	},
	"thread/status/changed": func(l *laneState, raw jsontext.Value) bool {
		note, err := decode[rpc.ThreadStatusChangedNotification](raw)
		return err == nil && l.edit(note.ThreadID, func(t *rpc.Thread) { t.Status = note.Status })
	},
	"thread/name/updated": func(l *laneState, raw jsontext.Value) bool {
		note, err := decode[rpc.ThreadNameUpdatedNotification](raw)
		return err == nil && l.edit(note.ThreadID, func(t *rpc.Thread) { t.Name = note.ThreadName })
	},
	"thread/archived": func(l *laneState, raw jsontext.Value) bool {
		note, err := decode[rpc.ThreadArchivedNotification](raw)
		return err == nil && l.archive(note.ThreadID)
	},
	"thread/unarchived": func(l *laneState, raw jsontext.Value) bool {
		note, err := decode[rpc.ThreadUnarchivedNotification](raw)
		return err == nil && l.unarchive(note.ThreadID)
	},
}

func (i LoadThreads) apply(m *machine) {
	ep := m.endpoint(i.Endpoint)
	if ep.lane.ticket != 0 {
		return
	}
	m.use(ep)
	if ep.live {
		m.loadLane(ep, false)
	}
}

func (m *machine) loadLane(ep *endpointState, reload bool) {
	lane := &ep.lane
	switch {
	case reload:
		lane.check, lane.err = laneCheck{}, nil
		m.listThreads(ep, "", false)
	case lane.err != nil:
		m.listThreads(ep, lane.request, lane.check.active)
	case !lane.loaded:
		m.listThreads(ep, "", false)
	case lane.more:
		m.listThreads(ep, lane.cursor, false)
	}
}

func (m *machine) listThreads(ep *endpointState, cursor string, check bool) {
	ep.lane.err, ep.lane.request = nil, cursor
	params := rpc.ThreadListParams{Cursor: cursor, Limit: lanePageLimit}
	ep.lane.ticket = m.call(ep, laneCall{link: ep.link, cursor: cursor, check: check}, methodThreadList, params)
	m.touchEndpoint(ep)
}

func (c laneCall) resume(m *machine, r resultMsg) {
	ep, ok := m.endpoints[c.link.Endpoint]
	if !ok || !ep.live || ep.link != c.link || ep.lane.ticket != r.ticket {
		return
	}
	lane := &ep.lane
	lane.ticket = 0
	m.touchEndpoint(ep)
	page, err := decodeResult[rpc.ThreadListResponse](r)
	if err != nil {
		lane.err = err
		return
	}
	switch {
	case c.check:
		m.checkedPage(ep, page)
		return
	case c.cursor == "":
		lane.rows, lane.pages = slices.Clone(page.Data), 1
	default:
		m.appendedPage(ep, page)
	}
	lane.loaded, lane.cursor, lane.more = true, page.NextCursor, page.NextCursor != ""
}

func decodeResult[T any](r resultMsg) (T, error) {
	if r.err != nil {
		var zero T
		return zero, r.err
	}
	return decode[T](r.raw)
}

func (m *machine) appendedPage(ep *endpointState, page rpc.ThreadListResponse) {
	lane := &ep.lane
	overlap := false
	rows := slices.Clip(lane.rows)
	for _, row := range page.Data {
		if slices.ContainsFunc(rows, func(t rpc.Thread) bool { return t.ID == row.ID }) {
			overlap = true
			continue
		}
		rows = append(rows, row)
	}
	lane.rows = rows
	lane.pages++
	if (overlap || page.NextCursor == "") && lane.pages > 1 {
		lane.check = laneCheck{active: true, left: lane.pages - 1}
		m.listThreads(ep, "", true)
	}
}

func (m *machine) checkedPage(ep *endpointState, page rpc.ThreadListResponse) {
	lane := &ep.lane
	if lane.matches(lane.check.at, page.Data) {
		lane.check = laneCheck{}
		return
	}
	kept := lane.merge(lane.check.at, page.Data)
	lane.check.at += kept
	lane.check.left--
	if lane.check.left > 0 && page.NextCursor != "" {
		m.listThreads(ep, page.NextCursor, true)
		return
	}
	lane.check = laneCheck{}
}

func (l *laneState) matches(at int, fresh []rpc.Thread) bool {
	if at+len(fresh) > len(l.rows) {
		return false
	}
	for i, row := range fresh {
		if l.rows[at+i].ID != row.ID || l.rows[at+i].UpdatedAt != row.UpdatedAt {
			return false
		}
	}
	return true
}

func (l *laneState) merge(at int, fresh []rpc.Thread) int {
	head := slices.Clone(l.rows[:min(at, len(l.rows))])
	seen := map[string]bool{}
	for _, row := range head {
		seen[row.ID] = true
	}
	kept := 0
	for _, row := range fresh {
		if !seen[row.ID] {
			seen[row.ID] = true
			head = append(head, row)
			kept++
		}
	}
	for _, row := range l.rows[min(at, len(l.rows)):] {
		if !seen[row.ID] {
			head = append(head, row)
		}
	}
	l.rows = head
	return kept
}

func (l *laneState) index(id string) int {
	return slices.IndexFunc(l.rows, func(t rpc.Thread) bool { return t.ID == id })
}

func (l *laneState) insert(row rpc.Thread) bool {
	rows := slices.Clone(l.rows)
	if at := l.index(row.ID); at >= 0 {
		rows = slices.Delete(rows, at, at+1)
	}
	at := slices.IndexFunc(rows, func(t rpc.Thread) bool { return t.UpdatedAt < row.UpdatedAt })
	if at < 0 {
		at = len(rows)
	}
	l.rows = slices.Insert(rows, at, row)
	return true
}

func (l *laneState) edit(id string, change func(*rpc.Thread)) bool {
	at := l.index(id)
	if at < 0 {
		return false
	}
	rows := slices.Clone(l.rows)
	change(&rows[at])
	l.rows = rows
	return true
}

func (l *laneState) archive(id string) bool {
	at := l.index(id)
	if at < 0 {
		return false
	}
	if l.archived == nil {
		l.archived = map[string]rpc.Thread{}
	}
	l.archived[id] = l.rows[at]
	l.rows = slices.Delete(slices.Clone(l.rows), at, at+1)
	return true
}

func (l *laneState) unarchive(id string) bool {
	row, ok := l.archived[id]
	if !ok {
		return false
	}
	delete(l.archived, id)
	return l.insert(row)
}

func (m *machine) updateLane(ep *endpointState, e envelope) {
	update, ok := laneUpdates[e.method]
	if ok && e.request == nil && e.params != nil && update(&ep.lane, e.params) {
		m.touchEndpoint(ep)
	}
}

func (m *machine) insertRow(ep *endpointState, row rpc.Thread) {
	ep.lane.insert(row)
	m.touchEndpoint(ep)
}

func (l *laneState) view() LaneView {
	return LaneView{Rows: l.rows, More: l.more, Loading: l.ticket != 0, Loaded: l.loaded, Err: l.err}
}
