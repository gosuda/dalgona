package state

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"math/rand/v2"
	"slices"
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

type truthThread struct {
	id      string
	name    string
	turns   []*truthTurn
	active  *truthTurn
	pending []string
}

type truthTurn struct {
	id     string
	status string
	items  []*truthItem
	open   *truthItem
}

type truthItem struct {
	id   string
	text []byte
	done bool
}

type frozenItem struct {
	id      string
	content rpc.ContentReference
}

type frozenTurn struct {
	id     string
	status string
	items  []frozenItem
}

type outEnvelope struct {
	first, last uint64
	thread      string
	method      string
	params      jsontext.Value
	spill       bool
	turn, item  string
	text        []byte
	request     *rpc.RequestParams
	sealed      bool
}

type fakeDaemon struct {
	id        client.EndpointID
	gen       client.Generation
	stream    string
	live      bool
	lostPhase client.Phase
	want      bool
	ordinal   uint64
	threads   map[string]*truthThread
	order     []string
	subs      map[string]bool
	outbox    []*outEnvelope
	inflight  []*outEnvelope
	snapshots map[string][]frozenTurn
	details   map[string][]byte
	cuts      map[string]uint64
	awaiting  map[string]bool
	delivered uint64
	ops       map[string]rpc.OperationRecord
}

type answerTarget struct {
	endpoint client.EndpointID
	gen      client.Generation
	thread   string
	request  string
}

type modelCoverage struct {
	coalesced, spilled, discardedLate, bufferedEarly, lateRequests, replacements, losses, answers, sends, reconciled int
}

type modelWorld struct {
	t       *testing.T
	rng     *rand.Rand
	m       *machine
	daemons []*fakeDaemon
	store   map[string][]byte
	calls   []effect
	results []resultMsg
	targets map[chan<- reply]answerTarget
	opens   map[uint64]ThreadKey
	serial  int
	resyncs int
	seen    *modelCoverage
}

var modelWindows = []WindowID{"w1", "w2"}

func newModelWorld(t *testing.T, seed uint64, seen *modelCoverage) *modelWorld {
	w := &modelWorld{
		t: t, rng: rand.New(rand.NewPCG(seed, seed^0x9e3779b97f4a7c15)), m: newMachine(),
		store: map[string][]byte{}, targets: map[chan<- reply]answerTarget{}, opens: map[uint64]ThreadKey{}, seen: seen,
	}
	for _, id := range []client.EndpointID{"local", "box"} {
		d := &fakeDaemon{id: id, threads: map[string]*truthThread{}, details: map[string][]byte{}, ops: map[string]rpc.OperationRecord{}}
		for _, thread := range []string{"t1", "t2"} {
			d.threads[thread] = &truthThread{id: thread}
			d.order = append(d.order, thread)
		}
		w.daemons = append(w.daemons, d)
	}
	return w
}

func (w *modelWorld) id(prefix string) string {
	w.serial++
	return prefix + strconv.Itoa(w.serial)
}

func (w *modelWorld) daemon(id client.EndpointID) *fakeDaemon {
	for _, d := range w.daemons {
		if d.id == id {
			return d
		}
	}
	w.t.Fatalf("no daemon %s", id)
	return nil
}

func (w *modelWorld) handle(msg message) {
	w.m.handle(msg)
	for _, e := range w.m.takeEffects() {
		w.effect(e)
	}
}

func (w *modelWorld) effect(e effect) {
	switch e := e.(type) {
	case callEffect:
		if e.method == methodClose || e.method == methodUnsubscribe {
			w.results = append(w.results, w.serve(e))
			return
		}
		w.calls = append(w.calls, e)
	case readEffect, operationEffect:
		w.calls = append(w.calls, e)
	case replyEffect:
		w.replied(e)
	case connectEffect:
		w.daemon(e.endpoint).want = true
	case resyncEffect:
		w.resyncs++
	case attachEffect, detachEffect, retireEffect, timerEffect, respondEffect, cancelEffect:
	}
}

func (w *modelWorld) contentRef(source rpc.ContentSource, field, encoding string, data []byte) rpc.ContentReference {
	id := w.id("c")
	w.store[id] = slices.Clone(data)
	sum := sha256.Sum256(data)
	return rpc.ContentReference{Source: source, ContentID: id, Field: field, Encoding: encoding, Total: rpc.FormatDecimal(uint64(len(data))), Digest: hex.EncodeToString(sum[:])}
}

func (w *modelWorld) header(source rpc.ContentSource, it *truthItem) rpc.ItemHeader {
	ref := w.contentRef(source, "text", "utf8", it.text)
	return rpc.ItemHeader{ID: it.id, Type: rpc.KindAgent, ItemRevision: ref.Digest[:16], Metadata: map[string]jsontext.Value{}, Contents: []rpc.ContentReference{ref}}
}

func mustJSON(t *testing.T, v any) jsontext.Value {
	raw, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

func (w *modelWorld) emit(d *fakeDaemon, thread, method string, params any) {
	if !d.live || !d.subs[thread] {
		return
	}
	d.ordinal++
	out := &outEnvelope{first: d.ordinal, last: d.ordinal, thread: thread, method: method, params: mustJSON(w.t, params)}
	out.spill = method == "item/completed" && w.rng.IntN(3) == 0
	d.outbox = append(d.outbox, out)
}

func (w *modelWorld) emitDelta(d *fakeDaemon, thread, turn, item string, text []byte) {
	if !d.live || !d.subs[thread] {
		return
	}
	d.ordinal++
	if n := len(d.outbox); n > 0 {
		tail := d.outbox[n-1]
		if !tail.sealed && tail.text != nil && tail.thread == thread && tail.item == item && tail.last+1 == d.ordinal {
			tail.last = d.ordinal
			tail.text = append(tail.text, text...)
			return
		}
	}
	d.outbox = append(d.outbox, &outEnvelope{first: d.ordinal, last: d.ordinal, thread: thread, method: "item/agentMessage/delta", turn: turn, item: item, text: slices.Clone(text)})
}

func (w *modelWorld) emitRequest(d *fakeDaemon, thread, request string) {
	if !d.live || !d.subs[thread] {
		return
	}
	d.ordinal++
	data := d.details[request]
	sum := sha256.Sum256(data)
	header := &rpc.RequestParams{
		Method:       "item/commandExecution/requestApproval",
		Details:      rpc.RequestDetails{RequestID: request, Total: rpc.FormatDecimal(uint64(len(data))), Digest: hex.EncodeToString(sum[:])},
		DalgonaEvent: rpc.EventPosition{StreamID: d.stream, Ordinal: rpc.FormatDecimal(d.ordinal), ThreadID: thread},
	}
	d.outbox = append(d.outbox, &outEnvelope{first: d.ordinal, last: d.ordinal, thread: thread, request: header})
}

var deltaAlphabet = []string{"a", "b", " ", "가", "é", "😀", "\n", "z", "日本"}

func (w *modelWorld) randomText() []byte {
	var b strings.Builder
	for range 1 + w.rng.IntN(4) {
		b.WriteString(deltaAlphabet[w.rng.IntN(len(deltaAlphabet))])
	}
	return []byte(b.String())
}

type daemonMove struct {
	weight int
	ready  func(th *truthThread) bool
	run    func(w *modelWorld, d *fakeDaemon, th *truthThread)
}

func idleThread(th *truthThread) bool { return th.active == nil }

func idleTurn(th *truthThread) bool { return th.active != nil && th.active.open == nil }

func streamingItem(th *truthThread) bool { return th.active != nil && th.active.open != nil }

var daemonMoves = []daemonMove{
	{3, idleThread, func(w *modelWorld, d *fakeDaemon, th *truthThread) { th.pending = append(th.pending, w.request(d, th)) }},
	{3, func(th *truthThread) bool { return idleThread(th) && len(th.pending) > 0 }, func(w *modelWorld, d *fakeDaemon, th *truthThread) {
		w.resolve(d, th, th.pending[w.rng.IntN(len(th.pending))])
	}},
	{2, idleThread, func(w *modelWorld, d *fakeDaemon, th *truthThread) {
		th.name = string(w.randomText())
		w.emit(d, th.id, "thread/name/updated", rpc.ThreadNameUpdatedNotification{ThreadID: th.id, ThreadName: th.name})
	}},
	{8, idleThread, (*modelWorld).startTurn},
	{2, idleTurn, (*modelWorld).completeTurn},
	{8, idleTurn, (*modelWorld).startItem},
	{3, streamingItem, (*modelWorld).completeItem},
	{15, streamingItem, func(w *modelWorld, d *fakeDaemon, th *truthThread) { w.appendDelta(d, th, w.randomText()) }},
}

func (w *modelWorld) act(d *fakeDaemon, th *truthThread) {
	ready := slices.DeleteFunc(slices.Clone(daemonMoves), func(m daemonMove) bool { return !m.ready(th) })
	weighted(w.rng, ready, func(m daemonMove) int { return m.weight }).run(w, d, th)
}

func (w *modelWorld) startTurn(d *fakeDaemon, th *truthThread) {
	th.active = &truthTurn{id: w.id("turn"), status: turnInProgress}
	th.turns = append(th.turns, th.active)
	w.emit(d, th.id, "turn/started", turnNote{Turn: liveTurn{ID: th.active.id, Status: turnInProgress}})
}

func (w *modelWorld) completeTurn(d *fakeDaemon, th *truthThread) {
	turn := th.active
	turn.status, th.active = "completed", nil
	w.emit(d, th.id, "turn/completed", turnNote{Turn: liveTurn{ID: turn.id, Status: "completed"}})
}

func (w *modelWorld) startItem(d *fakeDaemon, th *truthThread) {
	turn := th.active
	turn.open = &truthItem{id: w.id("item")}
	turn.items = append(turn.items, turn.open)
	w.emit(d, th.id, "item/started", itemNote{TurnID: turn.id, Item: w.header(rpc.ContentSource{Kind: "live", StreamID: d.stream}, turn.open)})
}

func (w *modelWorld) completeItem(d *fakeDaemon, th *truthThread) {
	turn := th.active
	it := turn.open
	it.done, turn.open = true, nil
	w.emit(d, th.id, "item/completed", itemNote{TurnID: turn.id, Item: w.header(rpc.ContentSource{Kind: "live", StreamID: d.stream}, it)})
}

func (w *modelWorld) appendDelta(d *fakeDaemon, th *truthThread, text []byte) {
	turn := th.active
	turn.open.text = append(turn.open.text, text...)
	w.emitDelta(d, th.id, turn.id, turn.open.id, text)
}

func (w *modelWorld) request(d *fakeDaemon, th *truthThread) string {
	id := w.id("req")
	d.details[id] = mustJSON(w.t, map[string]string{"threadId": th.id, "command": string(w.randomText())})
	w.emitRequest(d, th.id, id)
	return id
}

func (w *modelWorld) resolve(d *fakeDaemon, th *truthThread, request string) {
	if !slices.Contains(th.pending, request) {
		return
	}
	th.pending = slices.DeleteFunc(th.pending, func(id string) bool { return id == request })
	w.emit(d, th.id, methodResolved, rpc.ServerRequestResolvedNotification{ThreadID: th.id, RequestID: request})
}

func (w *modelWorld) eventParams(d *fakeDaemon, out *outEnvelope) jsontext.Value {
	params := out.params
	if out.text != nil {
		params = mustJSON(w.t, deltaNote{TurnID: out.turn, ItemID: out.item, Delta: string(out.text)})
	}
	event := rpc.EventParams{StreamID: d.stream, FirstOrdinal: rpc.FormatDecimal(out.first), LastOrdinal: rpc.FormatDecimal(out.last), ThreadID: out.thread, Method: out.method, Params: params}
	if out.spill {
		ref := w.contentRef(rpc.ContentSource{Kind: "live", StreamID: d.stream}, "params", "json", params)
		event.Params, event.ParamsContent = nil, &ref
	}
	return mustJSON(w.t, event)
}

func (w *modelWorld) deliverNext(d *fakeDaemon) bool {
	if len(d.outbox) == 0 {
		return false
	}
	out := d.outbox[0]
	d.outbox = d.outbox[1:]
	w.observe(d, out)
	link := client.Link{Endpoint: d.id, Generation: d.gen}
	if out.request != nil {
		w.handle(arrivalMsg{link: link, params: mustJSON(w.t, out.request)})
		d.inflight = append(d.inflight, out)
		return true
	}
	w.handle(notifyMsg{link: link, method: rpc.MethodEvent, params: w.eventParams(d, out)})
	return true
}

func (w *modelWorld) observe(d *fakeDaemon, out *outEnvelope) {
	d.delivered = out.last
	cut, opened := d.cuts[out.thread]
	switch {
	case out.last > out.first:
		w.seen.coalesced++
	case out.spill:
		w.seen.spilled++
	case opened && out.last <= cut:
		w.seen.discardedLate++
	case opened && d.awaiting[out.thread]:
		w.seen.bufferedEarly++
	}
}

func (w *modelWorld) deliverResult(r resultMsg) {
	if key, ok := w.opens[r.ticket]; ok {
		delete(w.opens, r.ticket)
		w.daemon(key.Endpoint).awaiting[key.Thread] = false
	}
	w.handle(r)
}

func (w *modelWorld) deliverRequest(d *fakeDaemon, at int) {
	out := d.inflight[at]
	d.inflight = slices.Delete(d.inflight, at, at+1)
	if d.delivered > out.first {
		w.seen.lateRequests++
	}
	waiter := make(chan reply, 1)
	w.targets[waiter] = answerTarget{endpoint: d.id, gen: d.gen, thread: out.thread, request: out.request.Details.RequestID}
	w.handle(awaitMsg{link: client.Link{Endpoint: d.id, Generation: d.gen}, request: out.request.Details.RequestID, waiter: waiter})
}

func (w *modelWorld) replied(e replyEffect) {
	target, ok := w.targets[e.to]
	if !ok {
		w.t.Fatal("a reply reached a waiter the transport never created")
	}
	delete(w.targets, e.to)
	d := w.daemon(target.endpoint)
	if e.reply.err == nil && d.live && d.gen == target.gen {
		w.seen.answers++
		w.resolve(d, d.threads[target.thread], target.request)
	}
}

func (w *modelWorld) serve(e effect) resultMsg {
	ticket, link := ticketOf(e), linkOf(e)
	d := w.daemon(link.Endpoint)
	if !d.live || d.gen != link.Generation {
		return resultMsg{ticket: ticket, endpoint: d.id, err: client.ErrStaleLink}
	}
	switch e := e.(type) {
	case callEffect:
		raw, err := w.call(d, e)
		return resultMsg{ticket: ticket, endpoint: d.id, raw: raw, err: err}
	case readEffect:
		return resultMsg{ticket: ticket, endpoint: d.id, data: w.read(d, e)}
	case operationEffect:
		raw, err := w.execute(d, e)
		return resultMsg{ticket: ticket, endpoint: d.id, raw: raw, err: err}
	}
	w.t.Fatalf("cannot serve %T", e)
	return resultMsg{}
}

func linkOf(e effect) client.Link {
	switch e := e.(type) {
	case callEffect:
		return e.link
	case readEffect:
		return e.link
	case operationEffect:
		return e.link
	}
	return client.Link{}
}

func (w *modelWorld) read(d *fakeDaemon, e readEffect) []byte {
	switch params := e.params("0").(type) {
	case detailsParams:
		return d.details[params.RequestID]
	case contentParams:
		return w.store[params.ContentID]
	}
	w.t.Fatalf("unexpected read %s", e.method)
	return nil
}

func (w *modelWorld) call(d *fakeDaemon, e callEffect) (jsontext.Value, error) {
	switch params := e.params.(type) {
	case openParams:
		w.opens[e.ticket] = ThreadKey{Endpoint: d.id, Thread: params.ThreadID}
		return mustJSON(w.t, w.open(d, params)), nil
	case pageParams:
		turns, ok := d.snapshots[params.HistoryID]
		if !ok {
			return nil, rpc.NativeKind(rpc.ErrorExpired)
		}
		return mustJSON(w.t, w.page(turns, params)), nil
	case historyParams:
		delete(d.snapshots, params.HistoryID)
		return mustJSON(w.t, map[string]bool{"closed": true}), nil
	case contentParams:
		data := w.store[params.ContentID]
		at, err := rpc.ParseDecimal(params.Offset)
		if err != nil {
			return nil, err
		}
		end := uint64(len(data))
		sum := sha256.Sum256(data)
		chunk := rpc.ContentChunk{DataBase64: base64.StdEncoding.EncodeToString(data[at:end]), Offset: params.Offset, Next: rpc.FormatDecimal(end), Total: rpc.FormatDecimal(uint64(len(data))), Digest: hex.EncodeToString(sum[:])}
		return mustJSON(w.t, chunk), nil
	case threadParams:
		delete(d.subs, params.ThreadID)
		return mustJSON(w.t, map[string]string{"status": "unsubscribed"}), nil
	}
	return w.callMore(d, e)
}

var endpointReads = map[string]string{
	methodThreadList:    `{"data":[]}`,
	methodModelList:     `{"data":[]}`,
	methodConfigRead:    `{"config":{},"origins":{}}`,
	methodSettingsRead:  `{"settings":[]}`,
	methodKeywordsMatch: `{"matches":[]}`,
}

func (w *modelWorld) callMore(d *fakeDaemon, e callEffect) (jsontext.Value, error) {
	if body, ok := endpointReads[e.method]; ok {
		return jsontext.Value(body), nil
	}
	params, ok := e.params.(operationParams)
	if !ok {
		w.t.Fatalf("unexpected call %s", e.method)
	}
	w.seen.reconciled++
	record, ok := d.ops[params.OperationID]
	if !ok {
		return nil, rpc.NativeKind(rpc.ErrorNotFound)
	}
	return mustJSON(w.t, record), nil
}

func (w *modelWorld) execute(d *fakeDaemon, e operationEffect) (jsontext.Value, error) {
	if _, sent := d.ops[e.op.OperationID]; sent {
		w.t.Fatalf("operation %s reached the daemon twice", e.op.OperationID)
	}
	params, ok := e.params.(rpc.TurnStartParams)
	if !ok || e.op.Method != methodTurnStart {
		w.t.Fatalf("unexpected operation %s", e.op.Method)
	}
	th := d.threads[params.ThreadID]
	if th.active != nil {
		d.ops[e.op.OperationID] = rpc.OperationRecord{State: "failed", Method: e.op.Method}
		return nil, &rpc.RPCError{Code: -32600, Message: "a turn is already running"}
	}
	w.startTurn(d, th)
	raw := mustJSON(w.t, turnNote{Turn: liveTurn{ID: th.active.id, Status: turnInProgress}})
	d.ops[e.op.OperationID] = rpc.OperationRecord{State: "succeeded", Method: e.op.Method, Result: raw}
	return raw, nil
}

func (w *modelWorld) send(key ThreadKey, window WindowID) {
	draft := DraftKey{Endpoint: key.Endpoint, Window: window, Thread: key.Thread}
	w.seen.sends++
	w.handle(Send{Key: draft, Draft: Draft{Text: string(w.randomText()), Revision: w.m.drafts[draft].Revision + 1}})
}

func (w *modelWorld) open(d *fakeDaemon, params openParams) rpc.HistorySnapshot {
	th := d.threads[params.ThreadID]
	if !d.subs[th.id] {
		d.subs[th.id] = true
		for _, id := range th.pending {
			w.emitRequest(d, th.id, id)
		}
	}
	if n := len(d.outbox); n > 0 {
		d.outbox[n-1].sealed = true
	}
	delete(d.snapshots, params.ReplaceHistoryID)
	if params.ReplaceHistoryID != "" {
		w.seen.replacements++
	}
	d.cuts[th.id], d.awaiting[th.id] = d.ordinal, true
	history := w.id("h")
	source := rpc.ContentSource{Kind: "snapshot", HistoryID: history}
	frozen := make([]frozenTurn, 0, len(th.turns))
	for _, turn := range slices.Backward(th.turns) {
		ft := frozenTurn{id: turn.id, status: turn.status}
		for _, it := range turn.items {
			ft.items = append(ft.items, frozenItem{id: it.id, content: w.contentRef(source, "text", "utf8", it.text)})
		}
		frozen = append(frozen, ft)
	}
	d.snapshots[history] = frozen
	snap := rpc.HistorySnapshot{
		HistoryID: history, Thread: rpc.Thread{ID: th.id, Name: th.name}, PathRevision: "path", StreamID: d.stream,
		CutOrdinal: rpc.FormatDecimal(d.ordinal), PendingRequests: []rpc.PendingRequest{},
	}
	if th.active != nil {
		snap.ActiveTurnID = th.active.id
	}
	for _, id := range th.pending {
		sum := sha256.Sum256(d.details[id])
		snap.PendingRequests = append(snap.PendingRequests, rpc.PendingRequest{RequestID: id, Method: "item/commandExecution/requestApproval", Details: rpc.ContentSummary{Total: rpc.FormatDecimal(uint64(len(d.details[id]))), Digest: hex.EncodeToString(sum[:])}})
	}
	return snap
}

func cursorText(turn, item int) string { return fmt.Sprintf("%d:%d", turn, item) }

func parseCursor(t *testing.T, cursor string) (turn, item int) {
	if cursor == "" {
		return 0, 0
	}
	if _, err := fmt.Sscanf(cursor, "%d:%d", &turn, &item); err != nil {
		t.Fatalf("cursor %q: %v", cursor, err)
	}
	return turn, item
}

func pagedTurn(ft frozenTurn, first, take int) rpc.HistoryTurn {
	turn := rpc.HistoryTurn{TurnID: ft.id, TurnRevision: "rev-" + ft.id, Status: ft.status, FirstItemIndex: first, NextItemIndex: first + take, ItemsComplete: first+take == len(ft.items)}
	for _, it := range ft.items[first : first+take] {
		turn.Items = append(turn.Items, rpc.ItemHeader{ID: it.id, Type: rpc.KindAgent, ItemRevision: it.content.Digest[:16], Metadata: map[string]jsontext.Value{}, Contents: []rpc.ContentReference{it.content}})
	}
	return turn
}

func (w *modelWorld) page(turns []frozenTurn, params pageParams) rpc.HistoryPage {
	ti, ii := parseCursor(w.t, params.Cursor)
	page := rpc.HistoryPage{Turns: []rpc.HistoryTurn{}, PathRevision: "path"}
	budget := 1 + w.rng.IntN(4)
	for ti < len(turns) && len(page.Turns) < params.Limit {
		turn := pagedTurn(turns[ti], ii, min(len(turns[ti].items)-ii, budget))
		page.Turns = append(page.Turns, turn)
		budget -= turn.NextItemIndex - ii
		if !turn.ItemsComplete {
			page.NextCursor = cursorText(ti, turn.NextItemIndex)
			return page
		}
		ti, ii = ti+1, 0
		if budget <= 0 {
			break
		}
	}
	if ti < len(turns) {
		page.NextCursor = cursorText(ti, 0)
	}
	return page
}

func (w *modelWorld) connect(d *fakeDaemon) {
	d.gen++
	d.stream = fmt.Sprintf("%s-%d", d.id, d.gen)
	d.live, d.want, d.ordinal = true, false, 0
	d.subs, d.outbox, d.inflight, d.snapshots = map[string]bool{}, nil, nil, map[string][]frozenTurn{}
	d.cuts, d.awaiting, d.delivered = map[string]uint64{}, map[string]bool{}, 0
	caps := rpc.Capabilities{ReceiptMethods: []string{methodTurnStart}}
	w.handle(phaseMsg{change: client.PhaseChange{Link: client.Link{Endpoint: d.id, Generation: d.gen}, Phase: client.PhaseConnected, Native: rpc.NativeSession{StreamID: d.stream}, Capabilities: caps}})
}

func (w *modelWorld) lose(d *fakeDaemon, phase client.Phase) {
	d.live, d.lostPhase, d.want = false, phase, false
	w.seen.losses++
	d.outbox, d.inflight = nil, nil
	w.handle(phaseMsg{change: client.PhaseChange{Link: client.Link{Endpoint: d.id, Generation: d.gen}, Phase: phase}})
}

type worldMove struct {
	weight int
	run    func(w *modelWorld, d *fakeDaemon, key ThreadKey, window WindowID)
}

var worldMoves = []worldMove{
	{30, func(w *modelWorld, d *fakeDaemon, key ThreadKey, _ WindowID) { w.act(d, d.threads[key.Thread]) }},
	{25, func(w *modelWorld, d *fakeDaemon, _ ThreadKey, _ WindowID) { w.deliverNext(d) }},
	{7, func(w *modelWorld, d *fakeDaemon, _ ThreadKey, _ WindowID) { w.deliverAnyRequest(d) }},
	{10, func(w *modelWorld, _ *fakeDaemon, _ ThreadKey, _ WindowID) { w.serveAny() }},
	{10, func(w *modelWorld, _ *fakeDaemon, _ ThreadKey, _ WindowID) { w.deliverAnyResult() }},
	{4, func(w *modelWorld, _ *fakeDaemon, key ThreadKey, window WindowID) {
		w.handle(OpenView{Window: window, Thread: key})
	}},
	{2, func(w *modelWorld, _ *fakeDaemon, key ThreadKey, window WindowID) {
		w.handle(CloseView{Window: window, Thread: key})
	}},
	{2, func(w *modelWorld, _ *fakeDaemon, key ThreadKey, _ WindowID) { w.handle(Refresh{Thread: key}) }},
	{2, func(w *modelWorld, _ *fakeDaemon, key ThreadKey, _ WindowID) { w.handle(LoadOlder{Thread: key}) }},
	{2, func(w *modelWorld, _ *fakeDaemon, _ ThreadKey, _ WindowID) { w.answerSome() }},
	{6, func(w *modelWorld, d *fakeDaemon, _ ThreadKey, _ WindowID) { w.transport(d) }},
	{3, func(w *modelWorld, _ *fakeDaemon, key ThreadKey, window WindowID) { w.send(key, window) }},
}

func weighted[T any](rng *rand.Rand, items []T, weight func(T) int) T {
	total := 0
	for _, item := range items {
		total += weight(item)
	}
	roll := rng.IntN(total)
	for _, item := range items {
		if roll < weight(item) {
			return item
		}
		roll -= weight(item)
	}
	return items[len(items)-1]
}

func (w *modelWorld) step() {
	d := w.daemons[w.rng.IntN(len(w.daemons))]
	key := ThreadKey{Endpoint: d.id, Thread: d.order[w.rng.IntN(len(d.order))]}
	window := modelWindows[w.rng.IntN(len(modelWindows))]
	weighted(w.rng, worldMoves, func(m worldMove) int { return m.weight }).run(w, d, key, window)
}

func (w *modelWorld) deliverAnyRequest(d *fakeDaemon) {
	if len(d.inflight) > 0 {
		w.deliverRequest(d, w.rng.IntN(len(d.inflight)))
	}
}

func (w *modelWorld) serveAny() {
	if len(w.calls) == 0 {
		return
	}
	at := w.rng.IntN(len(w.calls))
	call := w.calls[at]
	w.calls = slices.Delete(w.calls, at, at+1)
	w.results = append(w.results, w.serve(call))
}

func (w *modelWorld) deliverAnyResult() {
	if len(w.results) == 0 {
		return
	}
	at := w.rng.IntN(len(w.results))
	r := w.results[at]
	w.results = slices.Delete(w.results, at, at+1)
	w.deliverResult(r)
}

func (w *modelWorld) transport(d *fakeDaemon) {
	switch {
	case d.live && w.rng.IntN(2) == 0:
		w.lose(d, client.PhaseReconnecting)
	case d.live:
		w.lose(d, client.PhaseOffline)
	case d.want || d.lostPhase == client.PhaseReconnecting:
		w.connect(d)
	case d.lostPhase == client.PhaseOffline:
		w.handle(Connect{Endpoint: d.id})
	}
}

func (w *modelWorld) answerSome() {
	v := w.m.view()
	for _, key := range v.Threads() {
		tv, _ := v.Thread(key)
		for _, r := range tv.Requests {
			if r.Actionable {
				w.handle(Answer{Request: r.Key, Result: jsontext.Value(`{"decision":"accept"}`)})
				return
			}
		}
	}
}

func (w *modelWorld) settle() {
	for _, d := range w.daemons {
		for _, thread := range d.order {
			w.handle(OpenView{Window: "w1", Thread: ThreadKey{Endpoint: d.id, Thread: thread}})
		}
		if !d.live && d.lostPhase == client.PhaseOffline {
			w.handle(Connect{Endpoint: d.id})
		}
	}
	for round := 0; w.pump(); round++ {
		if round > 10000 {
			w.t.Fatal("the model never settled")
		}
	}
}

func (w *modelWorld) pump() bool {
	busy := false
	for _, d := range w.daemons {
		busy = w.pumpTransport(d) || busy
	}
	busy = w.pumpCalls() || busy
	v := w.m.view()
	for _, key := range v.Threads() {
		if tv, _ := v.Thread(key); tv.Live && !tv.HistoryComplete && !tv.LoadingOlder {
			w.handle(LoadOlder{Thread: key})
			busy = true
		}
	}
	return busy
}

func (w *modelWorld) pumpTransport(d *fakeDaemon) bool {
	busy := len(d.outbox)+len(d.inflight) > 0
	if !d.live && (d.want || d.lostPhase == client.PhaseReconnecting) {
		w.connect(d)
		busy = true
	}
	w.deliverOutbox(d)
	return busy
}

func (w *modelWorld) pumpCalls() bool {
	busy := false
	for len(w.calls) > 0 || len(w.results) > 0 {
		busy = true
		w.serveCalls()
		w.deliverResults()
	}
	return busy
}

func (w *modelWorld) resolveText(item ItemView) string {
	var text []byte
	for _, ref := range item.Header.Contents {
		if ref.Field == "text" {
			text = append(text, w.store[ref.ContentID]...)
		}
	}
	return string(append(text, item.Tail...))
}

func (w *modelWorld) verify() {
	if w.resyncs != 0 {
		w.t.Fatalf("a well-formed stream forced %d resyncs", w.resyncs)
	}
	for _, mu := range w.m.mutations {
		if mu.state != MutationFailed {
			w.t.Fatalf("mutation %s %s is still %d after settling", mu.id, mu.method, mu.state)
		}
	}
	v := w.m.view()
	for _, d := range w.daemons {
		for _, thread := range d.order {
			key := ThreadKey{Endpoint: d.id, Thread: thread}
			tv, ok := v.Thread(key)
			if !ok || !tv.Live || tv.Err != nil {
				w.t.Fatalf("%v is not live after settling: %+v", key, tv)
			}
			w.verifyThread(key, d.threads[thread], tv)
		}
	}
}

func (w *modelWorld) verifyThread(key ThreadKey, th *truthThread, tv *ThreadView) {
	if tv.Thread.Name != th.name {
		w.t.Fatalf("%v name %q, want %q", key, tv.Thread.Name, th.name)
	}
	if len(tv.Turns) != len(th.turns) {
		w.t.Fatalf("%v has %d turns, want %d", key, len(tv.Turns), len(th.turns))
	}
	for i, turn := range th.turns {
		w.verifyTurn(key, turn, tv.Turns[i])
	}
	w.verifyRequests(key, th, tv)
}

func (w *modelWorld) verifyTurn(key ThreadKey, turn *truthTurn, got TurnView) {
	if got.ID != turn.id || got.Status != turn.status || len(got.Items) != len(turn.items) {
		w.t.Fatalf("%v turn is %s/%s with %d items, want %s/%s with %d", key, got.ID, got.Status, len(got.Items), turn.id, turn.status, len(turn.items))
	}
	for j, it := range turn.items {
		if text := w.resolveText(got.Items[j]); got.Items[j].Key.Item != it.id || text != string(it.text) {
			w.t.Fatalf("%v item %s reads %q, want %s %q", key, got.Items[j].Key.Item, text, it.id, it.text)
		}
	}
}

func (w *modelWorld) verifyRequests(key ThreadKey, th *truthThread, tv *ThreadView) {
	ids := make([]string, 0, len(tv.Requests))
	for _, r := range tv.Requests {
		if !r.Actionable {
			w.t.Fatalf("%v request %s is not actionable: %+v", key, r.Key.Request, r)
		}
		ids = append(ids, r.Key.Request)
	}
	if !slices.Equal(ids, th.pending) {
		w.t.Fatalf("%v requests %v, want %v", key, ids, th.pending)
	}
}

func TestModelIncrementalProjectionEqualsTheDaemonFold(t *testing.T) {
	const interleavings = 1200
	seen := &modelCoverage{}
	for seed := range uint64(interleavings) {
		w := newModelWorld(t, seed, seen)
		for range 220 {
			w.step()
		}
		w.settle()
		w.verify()
	}
	t.Logf("covered %+v", *seen)
	for name, n := range map[string]int{
		"coalesced deltas": seen.coalesced, "spilled params": seen.spilled, "late pre-cut envelopes": seen.discardedLate,
		"envelopes buffered before a delayed snapshot": seen.bufferedEarly, "late request headers": seen.lateRequests,
		"replacement snapshots": seen.replacements, "transport losses": seen.losses, "answered requests": seen.answers,
		"sends": seen.sends, "reconciled receipts": seen.reconciled,
	} {
		if n == 0 {
			t.Errorf("no interleaving exercised %s", name)
		}
	}
}
