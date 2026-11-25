package state

import (
	"cmp"
	"encoding/json/jsontext"
	"fmt"
	"maps"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

const (
	defaultSubscriptions = 8
	methodResolved       = "serverRequest/resolved"
	resolutionTimedOut   = "timedOut"
	resolutionAnswered   = "answered"
)

type machine struct {
	endpoints  map[client.EndpointID]*endpointState
	drafts     map[DraftKey]Draft
	queues     map[DraftKey][]FollowUp
	mutations  []*mutation
	notices    []Notice
	calls      map[uint64]continuation
	serial     uint64
	effects    []effect
	cache      map[ThreadKey]cachedThread
	windows    map[WindowID][]ThreadKey
	arrivals   map[RequestKey]uint64
	slots      map[CallID]*callSlot
	keywords   map[DraftKey]*keywordState
	started    map[WindowID]ThreadKey
	meta       map[ThreadKey]threadMeta
	unsaved    map[DraftKey]bool
	workspaces map[workspaceKey]*workspace
	searches   map[DraftKey]*searchState
	shells     map[DraftKey][]*shellState
	dirty      bool
}

type endpointState struct {
	id         client.EndpointID
	label      string
	kind       client.EndpointKind
	phase      client.Phase
	link       client.Link
	stream     string
	caps       rpc.Capabilities
	err        error
	lost       bool
	live       bool
	connecting bool
	seq        sequencer
	threads    map[string]*threadState
	responders map[string]*responder
	workers    int
	backlog    []effect
	attach     attachment
	lane       laneState
	shared     sharedState
	version    uint64
}

type attachment struct {
	wanted  bool
	pending bool
	detach  func()
}

type threadState struct {
	key      ThreadKey
	interest map[WindowID]int
	cut      threadCut
	proj     *projection
	opening  uint64
	refresh  bool
	paging   uint64
	queue    []envelope
	fetching uint64
	err      error
	missing  bool
	starting string
	pages    pageStore
	version  uint64
}

func newMachine() *machine {
	return &machine{
		endpoints:  map[client.EndpointID]*endpointState{},
		drafts:     map[DraftKey]Draft{},
		queues:     map[DraftKey][]FollowUp{},
		calls:      map[uint64]continuation{},
		cache:      map[ThreadKey]cachedThread{},
		windows:    map[WindowID][]ThreadKey{},
		arrivals:   map[RequestKey]uint64{},
		slots:      map[CallID]*callSlot{},
		keywords:   map[DraftKey]*keywordState{},
		started:    map[WindowID]ThreadKey{},
		meta:       map[ThreadKey]threadMeta{},
		unsaved:    map[DraftKey]bool{},
		workspaces: map[workspaceKey]*workspace{},
		searches:   map[DraftKey]*searchState{},
		shells:     map[DraftKey][]*shellState{},
	}
}

func (m *machine) handle(msg message) { msg.apply(m) }

func (m *machine) takeEffects() []effect {
	out := m.effects
	m.effects = nil
	return out
}

func (m *machine) emit(e effect) { m.effects = append(m.effects, e) }

func (m *machine) next() uint64 {
	m.serial++
	return m.serial
}

func (m *machine) endpoint(id client.EndpointID) *endpointState {
	if ep, ok := m.endpoints[id]; ok {
		return ep
	}
	ep := &endpointState{id: id, threads: map[string]*threadState{}, responders: map[string]*responder{}}
	m.endpoints[id] = ep
	return ep
}

func (m *machine) seed(endpoints []client.EndpointStatus) {
	for _, status := range endpoints {
		ep := m.endpoint(status.ID)
		ep.label, ep.kind = status.Label, status.Kind
		m.touchEndpoint(ep)
	}
}

func (m *machine) label(id client.EndpointID) string {
	if ep, ok := m.endpoints[id]; ok && ep.label != "" {
		return ep.label
	}
	return string(id)
}

func (m *machine) thread(key ThreadKey) (*endpointState, *threadState) {
	ep, ok := m.endpoints[key.Endpoint]
	if !ok {
		return nil, nil
	}
	return ep, ep.threads[key.Thread]
}

func (m *machine) touch(ts *threadState) { ts.version = m.next() }

func (m *machine) touchEndpoint(ep *endpointState) { ep.version = m.next() }

func (m *machine) sortedThreads(ep *endpointState) []*threadState {
	return slices.SortedFunc(maps.Values(ep.threads), func(a, b *threadState) int { return cmp.Compare(a.key.Thread, b.key.Thread) })
}

func (m *machine) phase(c client.PhaseChange) {
	ep := m.endpoint(c.Link.Endpoint)
	ep.connecting = false
	if c.Phase == client.PhaseConnected {
		if !ep.live || c.Link != ep.link {
			m.connected(ep, c)
		}
		return
	}
	m.retire(ep)
	ep.phase, ep.err = c.Phase, c.Err
	ep.lost = ep.lost || lostPhase(c.Phase)
	if refusesSend(c.Phase) {
		cause := fmt.Errorf("%s is %s; reconnect to send", m.label(ep.id), c.Phase)
		m.abandonWaiting(ep, cause)
		m.abandonSlots(ep, cause)
	}
}

func (m *machine) connected(ep *endpointState, c client.PhaseChange) {
	m.retire(ep)
	ep.phase, ep.link, ep.stream, ep.caps = client.PhaseConnected, c.Link, c.Native.StreamID, c.Capabilities
	ep.err, ep.lost, ep.live = nil, false, true
	ep.seq = newSequencer()
	for _, ts := range m.sortedThreads(ep) {
		m.open(ep, ts)
	}
	m.reconcile(ep)
	m.dispatchWaiting(ep)
	m.loadLane(ep, true)
	m.loadShared(ep)
	m.dispatchSlots(ep)
	m.resumeWorkspaces(ep)
}

func (m *machine) retire(ep *endpointState) {
	if ep.live {
		m.emit(retireEffect{link: ep.link})
	}
	ep.live = false
	m.abandonResponders(ep)
	for _, e := range ep.backlog {
		delete(m.calls, ticketOf(e))
	}
	ep.backlog = nil
	ep.lane.ticket, ep.shared.pending = 0, 0
	m.expireWorkspaces(ep)
	for _, ts := range ep.threads {
		ts.cut, ts.opening, ts.refresh, ts.paging, ts.queue, ts.fetching = threadCut{}, 0, false, 0, nil, 0
		ts.pages = pageStore{}
		ts.proj.settle()
		m.touch(ts)
	}
	for _, mu := range m.mutations {
		if mu.endpoint() == ep.id && (mu.state == MutationDispatched || mu.state == MutationReconciling) {
			mu.state = MutationUnknown
		}
	}
	m.touchEndpoint(ep)
}

func (m *machine) resync(ep *endpointState, cause error) {
	m.retire(ep)
	ep.err, ep.lost = cause, true
	m.emit(resyncEffect{endpoint: ep.id})
}

func (m *machine) connectFailed(msg connectFailedMsg) {
	ep := m.endpoint(msg.endpoint)
	ep.connecting, ep.err = false, msg.err
	m.touchEndpoint(ep)
	m.notify(Notice{Endpoint: ep.id, Text: fmt.Sprintf("Could not connect to %s: %v", m.label(ep.id), msg.err)})
}

func (m *machine) use(ep *endpointState) {
	if ep.phase == client.PhaseIdle && !ep.connecting {
		ep.connecting = true
		m.emit(connectEffect{endpoint: ep.id})
	}
}

func (m *machine) notified(msg notifyMsg) {
	ep, ok := m.endpoints[msg.link.Endpoint]
	if !ok || !ep.live || ep.link != msg.link || msg.method != rpc.MethodEvent {
		return
	}
	ev, err := rpc.ParseEvent(ep.stream, msg.params)
	if err != nil {
		m.resync(ep, err)
		return
	}
	m.intake(ep, envelope{first: ev.First, last: ev.Last, thread: ev.ThreadID, method: ev.Method, params: ev.Params, content: ev.ParamsContent, at: msg.at})
}

func (m *machine) arrived(msg arrivalMsg) {
	ep, ok := m.endpoints[msg.link.Endpoint]
	if !ok || !ep.live || ep.link != msg.link {
		return
	}
	e, err := requestEnvelope(ep.stream, msg.params)
	if err != nil {
		m.resync(ep, err)
		return
	}
	m.intake(ep, e)
}

func requestEnvelope(stream string, raw jsontext.Value) (envelope, error) {
	header, err := decode[rpc.RequestParams](raw)
	if err != nil {
		return envelope{}, err
	}
	if header.DalgonaEvent.StreamID != stream || header.Details.RequestID == "" {
		return envelope{}, fmt.Errorf("%w: request %q names stream %q", rpc.ErrStreamBroken, header.Details.RequestID, header.DalgonaEvent.StreamID)
	}
	ordinal, err := rpc.ParseDecimal(header.DalgonaEvent.Ordinal)
	if err != nil || ordinal == 0 {
		return envelope{}, fmt.Errorf("%w: request ordinal %q", rpc.ErrStreamBroken, header.DalgonaEvent.Ordinal)
	}
	summary := rpc.ContentSummary{Total: header.Details.Total, Digest: header.Details.Digest}
	return envelope{
		first: ordinal, last: ordinal, thread: header.DalgonaEvent.ThreadID, method: header.Method,
		request: &arrival{id: header.Details.RequestID, method: header.Method, details: summary},
	}, nil
}

func (m *machine) intake(ep *endpointState, e envelope) {
	err := ep.seq.accept(e)
	if err == nil {
		err = m.deliver(ep, e)
	}
	if err != nil {
		m.resync(ep, err)
	}
}

func (m *machine) deliver(ep *endpointState, e envelope) error {
	m.track(ep, e)
	m.updateLane(ep, e)
	ts := m.route(ep, e)
	if ts == nil {
		return nil
	}
	now, err := ts.cut.route(e)
	if err != nil || !now {
		return err
	}
	return m.applyLive(ep, ts, e)
}

func (m *machine) route(ep *endpointState, e envelope) *threadState {
	if ts, ok := ep.threads[e.thread]; ok {
		return ts
	}
	if e.request == nil {
		return nil
	}
	for _, ts := range m.sortedThreads(ep) {
		if ts.proj != nil && ts.proj.parents(e.thread) {
			return ts
		}
	}
	return nil
}

func (m *machine) track(ep *endpointState, e envelope) {
	if e.request != nil {
		if _, ok := ep.responders[e.request.id]; !ok {
			ep.responders[e.request.id] = &responder{}
		}
		m.touchEndpoint(ep)
		return
	}
	if e.method != methodResolved || len(e.params) == 0 {
		return
	}
	note, err := decode[rpc.ServerRequestResolvedNotification](e.params)
	to, ok := ep.responders[note.RequestID]
	if err != nil || !ok {
		return
	}
	delete(ep.responders, note.RequestID)
	if !to.answered {
		m.answer(to, reply{err: rpc.NativeKind(rpc.ErrorCancelled)})
	}
	m.touchEndpoint(ep)
}

func (m *machine) applyLive(ep *endpointState, ts *threadState, e envelope) error {
	if ts.fetching == 0 && len(ts.queue) == 0 && e.content == nil {
		return m.applyProjection(ep, ts, e)
	}
	if len(ts.queue) >= mailboxEvents {
		return errMailbox
	}
	ts.queue = append(ts.queue, e)
	return m.drainQueue(ep, ts)
}

func (m *machine) drainQueue(ep *endpointState, ts *threadState) error {
	for ts.fetching == 0 && len(ts.queue) > 0 {
		head := ts.queue[0]
		if head.content != nil {
			ts.fetching = m.fetchParams(ep, ts, *head.content)
			return nil
		}
		ts.queue = ts.queue[1:]
		if err := m.applyProjection(ep, ts, head); err != nil {
			return err
		}
	}
	return nil
}

func (m *machine) applyProjection(ep *endpointState, ts *threadState, e envelope) error {
	err := ts.proj.apply(e)
	m.touch(ts)
	if err != nil {
		return err
	}
	switch {
	case e.request != nil:
		m.stamp(ep, ts)
		m.loadDetails(ep, ts)
	case e.method == methodResolved:
		m.announceResolution(ep, ts)
	case e.method == "turn/completed":
		m.progress(ep, ts)
	}
	return nil
}

func (m *machine) stamp(ep *endpointState, ts *threadState) {
	for _, r := range ts.proj.requests {
		if r.arrival != 0 {
			continue
		}
		key := RequestKey{Thread: ThreadKey{Endpoint: ep.id, Thread: r.owner(ts.key.Thread)}, Request: r.id}
		if _, ok := m.arrivals[key]; !ok {
			m.arrivals[key] = m.next()
		}
		r.arrival = m.arrivals[key]
	}
}

func (m *machine) announceResolution(ep *endpointState, ts *threadState) {
	r := ts.proj.lastResolved
	if r == nil {
		return
	}
	ts.proj.lastResolved = nil
	delete(m.arrivals, RequestKey{Thread: ThreadKey{Endpoint: ep.id, Thread: r.thread}, Request: r.id})
	switch {
	case r.resolution == resolutionTimedOut && r.method == methodQuestion:
		m.notify(Notice{Endpoint: ep.id, Thread: r.thread, Text: "Question timed out", Tone: NoticeStatus})
	case r.resolution == resolutionTimedOut:
		m.notify(Notice{Endpoint: ep.id, Thread: r.thread, Text: "Request timed out", Tone: NoticeStatus})
	case r.resolution == resolutionAnswered && !r.answeredHere:
		m.notify(Notice{Endpoint: ep.id, Thread: r.thread, Text: fmt.Sprintf("Another client answered a request on %s", m.label(ep.id)), Tone: NoticeStatus})
	}
}

func (m *machine) wantAttach(ep *endpointState) {
	ep.attach.wanted = true
	if ep.attach.detach == nil && !ep.attach.pending {
		ep.attach.pending = true
		m.emit(attachEffect{endpoint: ep.id})
	}
}

func (m *machine) dropAttach(ep *endpointState) {
	ep.attach.wanted = false
	if ep.attach.detach != nil {
		m.emit(detachEffect{detach: ep.attach.detach})
		ep.attach.detach = nil
	}
}

func (m *machine) attached(msg attachedMsg) {
	ep := m.endpoint(msg.endpoint)
	ep.attach.pending = false
	switch {
	case msg.err != nil:
		m.notify(Notice{Endpoint: ep.id, Text: fmt.Sprintf("Could not keep %s connected: %v", m.label(ep.id), msg.err)})
	case ep.attach.wanted:
		ep.attach.detach = msg.detach
	default:
		m.emit(detachEffect{detach: msg.detach})
	}
}
