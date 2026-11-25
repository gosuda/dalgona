package state

import (
	"bytes"
	"cmp"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

const (
	methodOpen          = "dalgona/history/open"
	methodPage          = "dalgona/history/page"
	methodClose         = "dalgona/history/close"
	methodContent       = "dalgona/history/content/read"
	methodDetails       = "dalgona/request/details/read"
	methodUnsubscribe   = "thread/unsubscribe"
	methodOperationRead = "dalgona/operation/read"
	readChunk           = 256 << 10
)

type continuation interface {
	resume(m *machine, r resultMsg)
}

type (
	openParams struct {
		ThreadID         string `json:"threadId"`
		ReplaceHistoryID string `json:"replaceHistoryId,omitzero"`
	}
	pageParams struct {
		HistoryID string `json:"historyId"`
		Cursor    string `json:"cursor,omitzero"`
		Direction string `json:"direction"`
		Limit     int    `json:"limit"`
	}
	historyParams struct {
		HistoryID string `json:"historyId"`
	}
	threadParams struct {
		ThreadID string `json:"threadId"`
	}
	contentParams struct {
		Source    rpc.ContentSource `json:"source"`
		ContentID string            `json:"contentId"`
		Offset    string            `json:"offset"`
		Limit     int               `json:"limit"`
	}
	detailsParams struct {
		RequestID string `json:"requestId"`
		Offset    string `json:"offset"`
		Limit     int    `json:"limit"`
	}
	operationParams struct {
		OperationID string `json:"operationId"`
	}
)

type (
	openCall struct {
		key     ThreadKey
		link    client.Link
		replace string
	}
	pageCall struct {
		key     ThreadKey
		link    client.Link
		history string
	}
	leaseCall struct {
		key     ThreadKey
		link    client.Link
		history string
	}
	releaseCall struct{}
	detailsCall struct {
		key     ThreadKey
		link    client.Link
		request string
		summary rpc.ContentSummary
	}
	contentCall struct {
		key     ThreadKey
		link    client.Link
		summary rpc.ContentSummary
	}
)

func ticketOf(e effect) uint64 {
	switch e := e.(type) {
	case callEffect:
		return e.ticket
	case readEffect:
		return e.ticket
	case operationEffect:
		return e.ticket
	}
	return 0
}

func (m *machine) spawn(ep *endpointState, c continuation, build func(ticket uint64) effect) uint64 {
	ticket := m.next()
	m.calls[ticket] = c
	e := build(ticket)
	if ep.workers >= workerLimit {
		ep.backlog = append(ep.backlog, e)
		return ticket
	}
	ep.workers++
	m.emit(e)
	return ticket
}

var callDeadlines = map[string]time.Duration{
	methodOpen: pageDeadline, methodPage: pageDeadline, methodContent: pageDeadline, methodImage: pageDeadline,
	methodWorkspaceOpen: decisionDeadline,
}

func (m *machine) call(ep *endpointState, c continuation, method string, params any) uint64 {
	deadline := cmp.Or(callDeadlines[method], metadataDeadline)
	link := ep.link
	return m.spawn(ep, c, func(ticket uint64) effect {
		return callEffect{ticket: ticket, link: link, method: method, params: params, deadline: deadline}
	})
}

func (m *machine) resume(r resultMsg) {
	c, ok := m.calls[r.ticket]
	if !ok {
		return
	}
	delete(m.calls, r.ticket)
	if ep, ok := m.endpoints[r.endpoint]; ok {
		m.freeWorker(ep)
	}
	c.resume(m, r)
}

func (m *machine) freeWorker(ep *endpointState) {
	ep.workers--
	if len(ep.backlog) > 0 {
		ep.workers++
		m.emit(ep.backlog[0])
		ep.backlog = ep.backlog[1:]
	}
	m.dispatchWaiting(ep)
}

func (m *machine) current(key ThreadKey, link client.Link) (*endpointState, *threadState, bool) {
	ep, ts := m.thread(key)
	if ts == nil || !ep.live || ep.link != link {
		return nil, nil, false
	}
	return ep, ts, true
}

func (m *machine) open(ep *endpointState, ts *threadState) {
	if ts.opening != 0 {
		ts.refresh = true
		return
	}
	params := openParams{ThreadID: ts.key.Thread}
	if ts.proj != nil && ts.proj.stream == ep.stream && ts.cut.phase == cutKnown {
		params.ReplaceHistoryID = ts.proj.historyID
	}
	ts.cut.reopen()
	ts.refresh = false
	ts.opening = m.call(ep, openCall{key: ts.key, link: ep.link, replace: params.ReplaceHistoryID}, methodOpen, params)
	m.touch(ts)
}

func (c openCall) resume(m *machine, r resultMsg) {
	ep, ts, ok := m.current(c.key, c.link)
	if !ok || ts.opening != r.ticket {
		m.releaseOrphan(c, r)
		return
	}
	ts.opening = 0
	if r.err != nil {
		m.openFailed(ep, ts, r.err)
		return
	}
	if err := m.establish(ep, ts, r.raw); err != nil {
		m.resync(ep, err)
		return
	}
	if ts.refresh {
		m.open(ep, ts)
	}
}

func (m *machine) releaseOrphan(c openCall, r resultMsg) {
	ep, ok := m.endpoints[c.key.Endpoint]
	if r.err != nil || !ok || !ep.live || ep.link != c.link {
		return
	}
	snap, err := decode[rpc.HistorySnapshot](r.raw)
	if err != nil {
		return
	}
	m.call(ep, releaseCall{}, methodClose, historyParams{HistoryID: snap.HistoryID})
	if _, interested := ep.threads[c.key.Thread]; !interested {
		m.call(ep, releaseCall{}, methodUnsubscribe, threadParams{ThreadID: c.key.Thread})
	}
}

func (releaseCall) resume(*machine, resultMsg) {}

func (m *machine) openFailed(ep *endpointState, ts *threadState, err error) {
	ts.cut.restore()
	m.touch(ts)
	data, _ := rpc.ErrorDataOf(err)
	if data.Kind == rpc.ErrorLimitExceeded {
		m.notify(Notice{Endpoint: ep.id, Thread: ts.key.Thread, Text: fmt.Sprintf("%s has no free history slot; close another thread view and try again", ep.id)})
		return
	}
	ts.err = err
	if data.Kind == rpc.ErrorNotFound {
		ts.proj, ts.missing = nil, true
	}
}

func (m *machine) establish(ep *endpointState, ts *threadState, raw jsontext.Value) error {
	snap, err := decode[rpc.HistorySnapshot](raw)
	if err != nil {
		return err
	}
	cut, err := rpc.ParseDecimal(snap.CutOrdinal)
	if err != nil || snap.StreamID != ep.stream {
		return fmt.Errorf("%w: snapshot for stream %q at cut %q", rpc.ErrStreamBroken, snap.StreamID, snap.CutOrdinal)
	}
	apply, err := ts.cut.establish(cut)
	if err != nil {
		return err
	}
	ts.proj, ts.queue, ts.fetching, ts.paging, ts.err, ts.missing = newProjection(snap, cut), nil, 0, 0, nil, false
	ts.pages = pageStore{}
	if ts.starting != snap.ActiveTurnID {
		ts.starting = ""
	}
	m.touch(ts)
	m.stamp(ep, ts)
	for _, e := range apply {
		if err := m.applyLive(ep, ts, e); err != nil {
			return err
		}
	}
	m.loadPage(ep, ts)
	m.loadDetails(ep, ts)
	m.emit(timerEffect{after: leaseInterval, fire: leaseMsg{thread: ts.key, history: snap.HistoryID}})
	m.progress(ep, ts)
	return nil
}

func (m *machine) loadPage(ep *endpointState, ts *threadState) {
	p := ts.proj
	if p == nil || p.complete || ts.paging != 0 || !ep.live {
		return
	}
	params := pageParams{HistoryID: p.historyID, Cursor: p.older, Direction: "older", Limit: historyPageTurns}
	ts.paging = m.call(ep, pageCall{key: ts.key, link: ep.link, history: p.historyID}, methodPage, params)
	m.touch(ts)
}

func (c pageCall) resume(m *machine, r resultMsg) {
	ep, ts, ok := m.current(c.key, c.link)
	if !ok || ts.paging != r.ticket || ts.proj == nil || ts.proj.historyID != c.history {
		return
	}
	ts.paging = 0
	m.touch(ts)
	if expired(r.err) {
		m.open(ep, ts)
		return
	}
	if r.err != nil {
		ts.err = r.err
		return
	}
	page, err := decode[rpc.HistoryPage](r.raw)
	if err == nil {
		err = ts.proj.mergePage(page)
	}
	ts.err = err
}

func expired(err error) bool {
	data, ok := rpc.ErrorDataOf(err)
	return ok && data.Kind == rpc.ErrorExpired
}

func (m *machine) renewLease(msg leaseMsg) {
	ep, ts := m.thread(msg.thread)
	if ts == nil || !ep.live || ts.proj == nil || ts.proj.historyID != msg.history {
		return
	}
	params := pageParams{HistoryID: msg.history, Direction: "older", Limit: 1}
	m.call(ep, leaseCall{key: ts.key, link: ep.link, history: msg.history}, methodPage, params)
	m.emit(timerEffect{after: leaseInterval, fire: msg})
}

func (c leaseCall) resume(m *machine, r resultMsg) {
	ep, ts, ok := m.current(c.key, c.link)
	if ok && expired(r.err) && ts.proj != nil && ts.proj.historyID == c.history {
		m.open(ep, ts)
	}
}

func (m *machine) release(ep *endpointState, ts *threadState) {
	delete(ep.threads, ts.key.Thread)
	delete(m.cache, ts.key)
	if ts.proj != nil {
		for _, rq := range ts.proj.requests {
			delete(m.arrivals, RequestKey{Thread: ThreadKey{Endpoint: ep.id, Thread: rq.owner(ts.key.Thread)}, Request: rq.id})
		}
	}
	if ep.live && ts.proj != nil && ts.proj.stream == ep.stream {
		m.call(ep, releaseCall{}, methodClose, historyParams{HistoryID: ts.proj.historyID})
	}
	if ep.live {
		m.call(ep, releaseCall{}, methodUnsubscribe, threadParams{ThreadID: ts.key.Thread})
	}
	m.releaseWorkspaces(ep, ts.key)
	if len(ep.threads) == 0 {
		m.dropAttach(ep)
	}
}

func (m *machine) loadDetails(ep *endpointState, ts *threadState) {
	if ts.proj == nil || !ep.live {
		return
	}
	for _, rq := range ts.proj.requests {
		if rq.params != nil || rq.loading || rq.err != nil {
			continue
		}
		rq.loading = true
		id, link, summary := rq.id, ep.link, rq.details
		m.spawn(ep, detailsCall{key: ts.key, link: link, request: id, summary: summary}, func(ticket uint64) effect {
			return readEffect{ticket: ticket, link: link, method: methodDetails, summary: summary, deadline: metadataDeadline, params: func(offset string) any {
				return detailsParams{RequestID: id, Offset: offset, Limit: readChunk}
			}}
		})
	}
	m.touch(ts)
}

func (c detailsCall) resume(m *machine, r resultMsg) {
	_, ts, ok := m.current(c.key, c.link)
	if !ok || ts.proj == nil {
		return
	}
	rq := ts.proj.request(c.request)
	if rq == nil || !rq.loading {
		return
	}
	rq.loading = false
	data, err := checkContent(c.summary, r.data, r.err)
	rq.params, rq.err = verifiedRequest(rq.owner(c.key.Thread), data, err)
	m.touch(ts)
}

var errRequestIdentity = errors.New("request details do not belong to this thread")

func verifiedRequest(thread string, data []byte, err error) (jsontext.Value, error) {
	if err != nil {
		return nil, err
	}
	if !bytes.HasPrefix(bytes.TrimSpace(data), []byte("{")) {
		return nil, errRequestIdentity
	}
	params, err := decode[threadParams](data)
	if err != nil {
		return nil, err
	}
	if params.ThreadID != "" && params.ThreadID != thread {
		return nil, errRequestIdentity
	}
	return jsontext.Value(data), nil
}

func (m *machine) fetchParams(ep *endpointState, ts *threadState, ref rpc.ContentReference) uint64 {
	link := ep.link
	summary := rpc.ContentSummary{Total: ref.Total, Digest: ref.Digest}
	return m.spawn(ep, contentCall{key: ts.key, link: link, summary: summary}, func(ticket uint64) effect {
		return readEffect{ticket: ticket, link: link, method: methodContent, summary: summary, deadline: pageDeadline, params: func(offset string) any {
			return contentParams{Source: ref.Source, ContentID: ref.ContentID, Offset: offset, Limit: readChunk}
		}}
	})
}

func (c contentCall) resume(m *machine, r resultMsg) {
	ep, ts, ok := m.current(c.key, c.link)
	if !ok || ts.fetching != r.ticket || len(ts.queue) == 0 {
		return
	}
	ts.fetching = 0
	if expired(r.err) {
		ts.queue = nil
		m.open(ep, ts)
		ts.cut.freeze()
		return
	}
	data, err := checkContent(c.summary, r.data, r.err)
	if err != nil {
		m.resync(ep, err)
		return
	}
	head := &ts.queue[0]
	head.params, head.content = data, nil
	if err := m.drainQueue(ep, ts); err != nil {
		m.resync(ep, err)
	}
}
