package state

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"slices"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	turnInProgress   = "inProgress"
	historyPageTurns = 20
)

var (
	errPageGap      = fmt.Errorf("%w: a history page skips items", rpc.ErrStreamBroken)
	errPageConflict = fmt.Errorf("%w: a history page repeats an item", rpc.ErrStreamBroken)
	errPageRevision = fmt.Errorf("%w: a history page changed revision inside one snapshot", rpc.ErrStreamBroken)
)

var deltaFields = map[string]string{
	"item/agentMessage/delta":           "text",
	"item/reasoning/textDelta":          "content",
	"item/commandExecution/outputDelta": "aggregatedOutput",
}

var appliers = map[string]func(*projection, jsontext.Value) error{
	"turn/started":                 func(p *projection, raw jsontext.Value) error { return p.applyTurn(raw, false) },
	"turn/completed":               func(p *projection, raw jsontext.Value) error { return p.applyTurn(raw, true) },
	"item/started":                 func(p *projection, raw jsontext.Value) error { return p.applyItem(raw, false) },
	"item/completed":               func(p *projection, raw jsontext.Value) error { return p.applyItem(raw, true) },
	"serverRequest/resolved":       (*projection).applyResolved,
	"thread/status/changed":        (*projection).applyStatus,
	"thread/name/updated":          (*projection).applyName,
	"thread/tokenUsage/updated":    (*projection).applyUsage,
	"error":                        (*projection).applyRetry,
	"turn/plan/updated":            (*projection).applyPlan,
	"dalgona/turn/effort":          (*projection).applyEffort,
	"dalgona/advisor/note":         (*projection).applyNote,
	"dalgona/thread/childSpawned":  (*projection).applyChild,
	"item/fileChange/patchUpdated": (*projection).applyPatch,
	"dalgona/sandbox/chosen":       (*projection).applySandbox,
	"warning":                      (*projection).applyWarning,
	"dalgona/tool/judged":          (*projection).applyToolReview,
	"dalgona/turn/judged":          (*projection).applyTurnReview,
}

type (
	liveTurn struct {
		ID          string         `json:"id"`
		Status      string         `json:"status"`
		Error       *rpc.TurnError `json:"error,omitzero"`
		StartedAt   int64          `json:"startedAt,omitzero"`
		CompletedAt int64          `json:"completedAt,omitzero"`
		DurationMs  int64          `json:"durationMs,omitzero"`
	}
	turnNote struct {
		Turn liveTurn `json:"turn"`
	}
	itemNote struct {
		TurnID string         `json:"turnId"`
		Item   rpc.ItemHeader `json:"item"`
	}
	deltaNote struct {
		TurnID string `json:"turnId"`
		ItemID string `json:"itemId"`
		Delta  string `json:"delta"`
	}
)

type projection struct {
	historyID    string
	pathRevision string
	stream       string
	cut          uint64
	applied      uint64
	thread       rpc.Thread
	activeTurn   string
	turns        []*turnState
	turnByID     map[string]*turnState
	requests     []*request
	usage        *rpc.ThreadTokenUsage
	older        string
	complete     bool
	partial      string
	notes        []AdvisorNote
	children     []childNote
	sandbox      string
	warning      string
	resolved     []resolvedNote
	lastResolved *resolvedNote
}

type turnState struct {
	id       string
	meta     liveTurn
	retry    *rpc.TurnError
	live     bool
	paged    bool
	revision string
	plan     []rpc.TurnPlanStep
	effort   string
	review   *Review
	waitFor  time.Duration
	waitAt   time.Time
	complete bool
	base     int
	items    []*itemState
	byID     map[string]*itemState
}

type itemState struct {
	id        string
	base      *rpc.ItemHeader
	live      *rpc.ItemHeader
	field     string
	tail      []byte
	changes   []rpc.FileChange
	review    *Review
	started   bool
	born      bool
	completed bool
}

type request struct {
	id       string
	thread   string
	method   string
	details  rpc.ContentSummary
	params   jsontext.Value
	loading  bool
	err      error
	answered bool
	warning  *AnswerWarning
	arrival  uint64
}

func (r *request) owner(thread string) string {
	if r.thread != "" {
		return r.thread
	}
	return thread
}

func newProjection(snap rpc.HistorySnapshot, cut uint64) *projection {
	p := &projection{
		historyID: snap.HistoryID, pathRevision: snap.PathRevision, stream: snap.StreamID,
		cut: cut, applied: cut, thread: snap.Thread, activeTurn: snap.ActiveTurnID,
		turnByID: map[string]*turnState{},
	}
	p.thread.Turns = nil
	for _, pending := range snap.PendingRequests {
		p.requests = append(p.requests, &request{id: pending.RequestID, thread: snap.Thread.ID, method: pending.Method, details: pending.Details})
	}
	return p
}

func decode[T any](raw jsontext.Value) (T, error) {
	var v T
	if err := json.Unmarshal(raw, &v); err != nil {
		return v, fmt.Errorf("%w: malformed event: %w", rpc.ErrStreamBroken, err)
	}
	return v, nil
}

func (p *projection) apply(e envelope) error {
	p.applied = e.last
	if e.request != nil {
		p.present(e.request, e.thread)
		return nil
	}
	if e.method == methodRetryWait {
		return p.applyRetryWait(e.params, e.at)
	}
	if field, ok := deltaFields[e.method]; ok {
		return p.applyDelta(field, e.params)
	}
	if apply, ok := appliers[e.method]; ok {
		return apply(p, e.params)
	}
	return nil
}

func (p *projection) turn(id string) *turnState {
	if t, ok := p.turnByID[id]; ok {
		return t
	}
	t := &turnState{id: id, byID: map[string]*itemState{}}
	p.turnByID[id] = t
	p.turns = append(p.turns, t)
	return t
}

func (p *projection) finished(id string) bool {
	t, ok := p.turnByID[id]
	return ok && t.final()
}

func (t *turnState) item(id string) *itemState {
	if it, ok := t.byID[id]; ok {
		return it
	}
	it := &itemState{id: id}
	t.byID[id] = it
	t.items = append(t.items, it)
	return it
}

func (t *turnState) final() bool { return t.meta.Status != "" && t.meta.Status != turnInProgress }

func (p *projection) applyTurn(raw jsontext.Value, completed bool) error {
	note, err := decode[turnNote](raw)
	if err != nil {
		return err
	}
	t := p.turn(note.Turn.ID)
	if !completed && t.final() {
		return nil
	}
	t.meta, t.live = note.Turn, true
	switch {
	case !completed:
		p.activeTurn = t.id
	case p.activeTurn == t.id:
		p.activeTurn = ""
	}
	if completed {
		t.waitFor, t.waitAt = 0, time.Time{}
	}
	return nil
}

func (p *projection) applyItem(raw jsontext.Value, completed bool) error {
	note, err := decode[itemNote](raw)
	if err != nil {
		return err
	}
	it := p.turn(note.TurnID).item(note.Item.ID)
	if it.completed {
		return nil
	}
	header := note.Item
	it.live, it.tail, it.started, it.completed = &header, nil, !completed, completed
	it.born = it.born || !completed
	return nil
}

func (p *projection) applyDelta(field string, raw jsontext.Value) error {
	note, err := decode[deltaNote](raw)
	if err != nil {
		return err
	}
	it := p.turn(note.TurnID).item(note.ItemID)
	if it.completed {
		return nil
	}
	it.field = field
	it.tail = append(it.tail, note.Delta...)
	return nil
}

func (p *projection) applyResolved(raw jsontext.Value) error {
	note, err := decode[rpc.ServerRequestResolvedNotification](raw)
	if err != nil {
		return err
	}
	if r := p.request(note.RequestID); r != nil {
		p.resolve(r, note.Resolution)
	}
	p.requests = slices.DeleteFunc(p.requests, func(r *request) bool { return r.id == note.RequestID })
	return nil
}

func (p *projection) applyStatus(raw jsontext.Value) error {
	note, err := decode[rpc.ThreadStatusChangedNotification](raw)
	p.thread.Status = note.Status
	return err
}

func (p *projection) applyName(raw jsontext.Value) error {
	note, err := decode[rpc.ThreadNameUpdatedNotification](raw)
	p.thread.Name = note.ThreadName
	return err
}

func (p *projection) applyUsage(raw jsontext.Value) error {
	note, err := decode[rpc.ThreadTokenUsageUpdatedNotification](raw)
	p.usage = &note.TokenUsage
	return err
}

func (p *projection) applyRetry(raw jsontext.Value) error {
	note, err := decode[rpc.ErrorNotification](raw)
	if err != nil {
		return err
	}
	t := p.turn(note.TurnID)
	if note.WillRetry {
		t.retry = &note.Error
		return nil
	}
	t.meta.Error = &note.Error
	return nil
}

func (p *projection) present(a *arrival, thread string) {
	if slices.ContainsFunc(p.requests, func(r *request) bool { return r.id == a.id }) {
		return
	}
	p.requests = append(p.requests, &request{id: a.id, thread: thread, method: a.method, details: a.details})
}

func (p *projection) parents(child string) bool {
	return slices.ContainsFunc(p.children, func(c childNote) bool { return c.ID == child })
}

func (p *projection) request(id string) *request {
	for _, r := range p.requests {
		if r.id == id {
			return r
		}
	}
	return nil
}

func (p *projection) mergePage(page rpc.HistoryPage) error {
	if page.PathRevision != p.pathRevision {
		return errPageRevision
	}
	for _, h := range page.Turns {
		if err := p.mergeTurn(h); err != nil {
			return err
		}
	}
	p.older, p.complete = page.NextCursor, page.NextCursor == ""
	return nil
}

func (p *projection) mergeTurn(h rpc.HistoryTurn) error {
	t, known := p.turnByID[h.TurnID]
	if err := p.continues(t, known, h); err != nil {
		return err
	}
	if !known {
		t = &turnState{id: h.TurnID, byID: map[string]*itemState{}}
		p.turnByID[t.id] = t
		p.turns = slices.Insert(p.turns, 0, t)
	}
	for _, header := range h.Items {
		if err := t.mergeItem(header); err != nil {
			return err
		}
	}
	t.paged, t.revision, t.complete = true, h.TurnRevision, h.ItemsComplete
	if !t.live {
		t.meta = liveTurn{ID: h.TurnID, Status: h.Status, Error: h.Error, StartedAt: h.StartedAt, CompletedAt: h.CompletedAt, DurationMs: h.DurationMs}
	}
	p.partial = ""
	if !h.ItemsComplete {
		p.partial = t.id
	}
	return nil
}

func (p *projection) continues(t *turnState, known bool, h rpc.HistoryTurn) error {
	switch {
	case h.NextItemIndex != h.FirstItemIndex+len(h.Items), p.partial != "" && p.partial != h.TurnID:
		return errPageGap
	case known:
		return t.accepts(h)
	case h.FirstItemIndex != 0:
		return errPageGap
	}
	return nil
}

func (t *turnState) accepts(h rpc.HistoryTurn) error {
	switch {
	case t.paged && t.revision != h.TurnRevision:
		return errPageRevision
	case t.paged && t.complete:
		return errPageConflict
	case h.FirstItemIndex != t.base:
		return errPageGap
	}
	return nil
}

func (t *turnState) mergeItem(header rpc.ItemHeader) error {
	it, ok := t.byID[header.ID]
	switch {
	case !ok:
		it = &itemState{id: header.ID}
		t.byID[it.id] = it
		t.items = slices.Insert(t.items, t.base, it)
	case it.base != nil || it.born || slices.Index(t.items, it) != t.base:
		return errPageConflict
	}
	it.base = &header
	t.base++
	return nil
}

func (p *projection) settle() {
	if p == nil {
		return
	}
	for _, r := range p.requests {
		r.loading = false
	}
}

func (it *itemState) header() rpc.ItemHeader {
	switch {
	case it.live != nil:
		return *it.live
	case it.base != nil:
		return *it.base
	}
	return rpc.ItemHeader{ID: it.id}
}
