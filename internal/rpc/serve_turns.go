package rpc

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"slices"
	"strconv"

	"github.com/gosuda/dalgona/internal/agent/event"
)

const (
	turnsDefaultLimit = 20
	turnsMaxLimit     = 100
)

type replayer func(t *tracker, path []event.Seqd) []Turn

type turnCursor struct {
	raw       string
	seq       uint64
	inclusive bool
}

type turnPageQuery struct {
	cursor    *turnCursor
	limit     int
	ascending bool
	notLoaded bool
}

type turnSpan struct {
	seq        uint64
	start, end int
}

func (s *server) threadTurnsList(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p ThreadTurnsListParams
	if err := decodeParams(params, p.member); err != nil {
		return nil, err
	}
	query, err := newTurnPageQuery(p)
	if err != nil {
		return nil, err
	}
	thread, err := s.open(ctx, p.ThreadID)
	if err != nil {
		return nil, err
	}
	records, _, err := thread.Records()
	if err != nil {
		return nil, hostError(err, p.ThreadID, "")
	}
	page, err := turnsPage(records, s.newTracker(p.ThreadID), query)
	if err != nil {
		return nil, err
	}
	return encode(page)
}

func (p *ThreadTurnsListParams) member(name string, d *jsontext.Decoder) error {
	var err error
	switch name {
	case "threadId":
		p.ThreadID, err = readString(d)
	case "cursor":
		p.Cursor, err = readString(d)
	case "limit":
		p.Limit, err = readInt(d)
	case "sortDirection":
		p.SortDirection, err = readString(d)
	case "itemsView":
		p.ItemsView, err = readString(d)
	default:
		err = d.SkipValue()
	}
	return err
}

func newTurnPageQuery(p ThreadTurnsListParams) (turnPageQuery, error) {
	if p.Limit < 0 {
		return turnPageQuery{}, invalidParams("invalid limit " + strconv.Itoa(p.Limit))
	}
	query := turnPageQuery{limit: min(cmp.Or(p.Limit, turnsDefaultLimit), turnsMaxLimit)}
	var err error
	if query.ascending, err = sortsAscending(p.SortDirection); err != nil {
		return turnPageQuery{}, err
	}
	if query.notLoaded, err = omitsItems(p.ItemsView); err != nil {
		return turnPageQuery{}, err
	}
	if p.Cursor == "" {
		return query, nil
	}
	cursor, err := parseTurnCursor(p.Cursor)
	if err != nil {
		return turnPageQuery{}, err
	}
	query.cursor = &cursor
	return query, nil
}

func sortsAscending(direction string) (bool, error) {
	switch direction {
	case "", "desc":
		return false, nil
	case "asc":
		return true, nil
	}
	return false, invalidParams("unsupported sortDirection " + direction)
}

func omitsItems(view string) (bool, error) {
	switch view {
	case "", "full", "summary":
		return false, nil
	case "notLoaded":
		return true, nil
	}
	return false, invalidParams("unsupported itemsView " + view)
}

func parseTurnCursor(raw string) (turnCursor, error) {
	if len(raw) < 2 {
		return turnCursor{}, invalidParams("invalid cursor " + raw)
	}
	cursor := turnCursor{raw: raw}
	switch raw[0] {
	case 'a':
	case 'b':
		cursor.inclusive = true
	default:
		return turnCursor{}, invalidParams("invalid cursor " + raw)
	}
	digits := raw[1:]
	seq, err := strconv.ParseUint(digits, 10, 64)
	if err != nil || strconv.FormatUint(seq, 10) != digits {
		return turnCursor{}, invalidParams("invalid cursor " + raw)
	}
	cursor.seq = seq
	return cursor, nil
}

func turnSpans(path []event.Seqd) []turnSpan {
	var spans []turnSpan
	for i, e := range path {
		if _, ok := e.Event.(event.TurnStarted); !ok {
			continue
		}
		if n := len(spans); n > 0 {
			spans[n-1].end = i
		}
		spans = append(spans, turnSpan{seq: e.Seq, start: i, end: len(path)})
	}
	return spans
}

func (q turnPageQuery) window(spans []turnSpan) (int, int, error) {
	total := len(spans)
	start := 0
	if !q.ascending {
		start = total - 1
	}
	if q.cursor != nil {
		anchor, ok := slices.BinarySearchFunc(spans, q.cursor.seq, func(span turnSpan, seq uint64) int {
			return cmp.Compare(span.seq, seq)
		})
		if !ok {
			return 0, 0, invalidParams("cursor " + q.cursor.raw + " is not on the active path")
		}
		start = anchor
		if !q.cursor.inclusive {
			start = q.step(anchor)
		}
	}
	if q.ascending {
		low := min(start, total)
		return low, min(low+q.limit, total), nil
	}
	high := start + 1
	return max(high-q.limit, 0), high, nil
}

func (q turnPageQuery) step(from int) int {
	if q.ascending {
		return from + 1
	}
	return from - 1
}

func turnsPage(records []event.Seqd, tr *tracker, q turnPageQuery) (ThreadTurnsListResponse, error) {
	path := event.ActivePath(records)
	spans := turnSpans(path)
	low, high, err := q.window(spans)
	if err != nil {
		return ThreadTurnsListResponse{}, err
	}
	page := ThreadTurnsListResponse{Data: []Turn{}}
	if low == high {
		return page, nil
	}
	if created, ok := sessionCreated(records); ok {
		tr.created = created
	}
	collect := tr.collectTurns
	if q.notLoaded {
		collect = tr.collectTurnHeaders
	}
	page.Data = collect(path[spans[low].start:spans[high-1].end])
	first, last, more := spans[low].seq, spans[high-1].seq, high < len(spans)
	if !q.ascending {
		slices.Reverse(page.Data)
		first, last, more = last, first, low > 0
	}
	page.BackwardsCursor = "b" + strconv.FormatUint(first, 10)
	if more {
		page.NextCursor = "a" + strconv.FormatUint(last, 10)
	}
	return page, nil
}

func sessionCreated(records []event.Seqd) (event.SessionCreated, bool) {
	if len(records) == 0 {
		return event.SessionCreated{}, false
	}
	created, ok := records[0].Event.(event.SessionCreated)
	return created, ok
}

func (t *tracker) collectTurns(events []event.Seqd) []Turn {
	return t.collect(events, func(event.Event) bool { return true })
}

func (t *tracker) collectTurnHeaders(events []event.Seqd) []Turn {
	return t.collect(events, turnBoundary)
}

func (t *tracker) collect(events []event.Seqd, keep func(event.Event) bool) []Turn {
	c := &collector{}
	t.sink = c
	for _, e := range events {
		if keep(e.Event) {
			t.apply(e.Seq, e.Event)
		}
	}
	return c.result()
}

func turnBoundary(ev event.Event) bool {
	_, started := ev.(event.TurnStarted)
	_, completed := ev.(event.TurnCompleted)
	return started || completed
}

func (t *tracker) replayAll(path []event.Seqd) []Turn {
	return t.collectTurns(path)
}

func (t *tracker) replayLive(path []event.Seqd) []Turn {
	open := openTurnStart(path)
	for _, e := range path[:open] {
		t.applyHistory(e.Seq, e.Event)
	}
	t.collectTurns(path[open:])
	return nil
}

func openTurnStart(path []event.Seqd) int {
	open := len(path)
	for i, e := range path {
		switch e.Event.(type) {
		case event.TurnStarted:
			open = i
		case event.TurnCompleted:
			open = len(path)
		}
	}
	return open
}

func (t *tracker) applyHistory(seq uint64, ev event.Event) {
	t.seq = seq
	switch e := ev.(type) {
	case event.TurnStarted:
		t.beginTurn(e)
	case event.TurnCompleted:
		t.endTurn()
	default:
		t.applyUsage(ev)
	}
}

func (r ThreadTurnsListResponse) write(o *out) {
	o.open()
	o.key("data")
	o.openArray()
	for _, turn := range r.Data {
		turn.write(o)
	}
	o.closeArray()
	o.optStr("nextCursor", r.NextCursor)
	o.optStr("backwardsCursor", r.BackwardsCursor)
	o.close()
}
