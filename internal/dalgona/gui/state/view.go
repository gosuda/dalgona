package state

import (
	"cmp"
	"encoding/json/jsontext"
	"maps"
	"slices"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

type View struct {
	endpoints []EndpointView
	lanes     map[client.EndpointID]LaneView
	threads   map[ThreadKey]*ThreadView
	order     []ThreadKey
	windows   []WindowView
	drafts    map[DraftKey]Draft
	followUps map[DraftKey][]FollowUp
	mutations []MutationView
	notices   []Notice
	started   map[WindowID]ThreadKey
	calls     map[CallID]CallView
	keywords  map[DraftKey]DraftKeywords
	unsaved   map[DraftKey]bool
	searches  map[DraftKey]FileSearch
	shells    map[DraftKey][]ShellRun
}

type Snapshot struct {
	Endpoints []EndpointView
	Lanes     map[client.EndpointID]LaneView
	Threads   []*ThreadView
	Drafts    map[DraftKey]Draft
	FollowUps map[DraftKey][]FollowUp
	Mutations []MutationView
	Notices   []Notice
	Calls     []CallView
}

type EndpointView struct {
	ID           client.EndpointID
	Label        string
	Kind         client.EndpointKind
	Phase        client.Phase
	Link         client.Link
	Live         bool
	Offline      bool
	Err          error
	Capabilities rpc.Capabilities
	Threads      int
	Models       []rpc.Model
	Config       rpc.ConfigValues
	Settings     []rpc.Setting
	Approval     string
	Verbosity    string
	StatusHide   []string
	SettingsErr  error
}

type WindowView struct {
	ID      WindowID
	Threads []ThreadKey
}

type Cost struct {
	USDMicros int64
	Partial   bool
}

type ThreadView struct {
	Key             ThreadKey
	Phase           client.Phase
	Live            bool
	Offline         bool
	Opening         bool
	Missing         bool
	Err             error
	Link            client.Link
	HistoryID       string
	Thread          rpc.Thread
	ActiveTurn      string
	Turns           []TurnView
	Requests        []RequestView
	Resolved        []ResolvedRequest
	Usage           *rpc.ThreadTokenUsage
	Cost            *Cost
	Model           string
	Approval        string
	Effort          string
	HistoryComplete bool
	LoadingOlder    bool
	Cut             uint64
	Notes           []AdvisorNote
	Children        []Child
	Sandbox         string
	Warning         string
	Applied         uint64
	contents        map[ContentKey]ContentPage
	images          map[ImageKey]ImagePage
}

type TurnView struct {
	ID            string
	Status        string
	Error         *rpc.TurnError
	RetryError    *rpc.TurnError
	RetryWait     time.Duration
	RetryWaitAt   time.Time
	StartedAt     int64
	CompletedAt   int64
	DurationMs    int64
	Paged         bool
	ItemsComplete bool
	Items         []ItemView
	Plan          []rpc.TurnPlanStep
	Effort        string
	Review        *Review
}

type ItemView struct {
	Key         ItemKey
	Header      rpc.ItemHeader
	Field       string
	Tail        []byte
	Provisional bool
	Changes     []rpc.FileChange
	Review      *Review
}

type RequestView struct {
	Key        RequestKey
	Method     string
	Details    rpc.ContentSummary
	Params     jsontext.Value
	Loading    bool
	Err        error
	Answered   bool
	Actionable bool
	Warning    *AnswerWarning
	Arrival    uint64
}

type MutationView struct {
	OperationID string
	Key         DraftKey
	Method      string
	State       MutationState
	Payload     Draft
	Err         error
	Progress    string
	Receipt     bool
}

type Decision struct {
	Request  RequestKey
	Method   string
	Answered bool
}

type cachedThread struct {
	version  uint64
	endpoint uint64
	view     *ThreadView
}

func (s Snapshot) View() *View {
	v := &View{
		endpoints: s.Endpoints, lanes: s.Lanes, threads: map[ThreadKey]*ThreadView{}, drafts: s.Drafts,
		followUps: s.FollowUps, mutations: s.Mutations, notices: s.Notices, calls: map[CallID]CallView{},
	}
	for _, c := range s.Calls {
		v.calls[c.ID] = c
	}
	for _, t := range s.Threads {
		v.threads[t.Key] = t
		v.order = append(v.order, t.Key)
	}
	return v
}

func (v *View) Endpoints() []EndpointView { return v.endpoints }

func (v *View) Lane(id client.EndpointID) LaneView { return v.lanes[id] }

func (v *View) Thread(key ThreadKey) (*ThreadView, bool) {
	t, ok := v.threads[key]
	return t, ok
}

func (v *View) Threads() []ThreadKey { return v.order }

func (v *View) Windows() []WindowView { return v.windows }

func (v *View) Started(window WindowID) (ThreadKey, bool) {
	key, ok := v.started[window]
	return key, ok
}

func (v *View) Draft(key DraftKey) Draft { return v.drafts[key] }

func (v *View) WindowDrafts(window WindowID) []DraftKey {
	var keys []DraftKey
	for key, d := range v.drafts {
		if key.Window == window && !d.empty() {
			keys = append(keys, key)
		}
	}
	for key, queue := range v.followUps {
		if key.Window == window && len(queue) > 0 && !slices.Contains(keys, key) {
			keys = append(keys, key)
		}
	}
	slices.SortFunc(keys, func(a, b DraftKey) int {
		return cmp.Or(cmp.Compare(a.Endpoint, b.Endpoint), cmp.Compare(a.Thread, b.Thread))
	})
	return keys
}

func (v *View) Unsaved(key DraftKey) bool { return v.unsaved[key] }

func (v *View) FollowUps(key DraftKey) []FollowUp { return v.followUps[key] }

func (v *View) Mutations() []MutationView { return v.mutations }

func (v *View) Notices() []Notice { return v.notices }

func (v *View) Call(id CallID) (CallView, bool) {
	c, ok := v.calls[id]
	return c, ok
}

func (v *View) DraftKeywords(key DraftKey) DraftKeywords { return v.keywords[key] }

func (v *View) FileSearch(key DraftKey) FileSearch { return v.searches[key] }

func (v *View) ShellRuns(key DraftKey) []ShellRun { return v.shells[key] }

func (v *View) Summary(key ThreadKey) (rpc.Thread, bool) {
	rows := v.lanes[key.Endpoint].Rows
	at := slices.IndexFunc(rows, func(t rpc.Thread) bool { return t.ID == key.Thread })
	if at < 0 {
		return rpc.Thread{}, false
	}
	return rows[at], true
}

func (v *View) AcceptsImages(key ThreadKey) (accepts, known bool) {
	at := slices.IndexFunc(v.endpoints, func(e EndpointView) bool { return e.ID == key.Endpoint })
	if at < 0 {
		return false, false
	}
	model := v.endpoints[at].Config.Model
	if t, ok := v.threads[key]; ok && t.Model != "" {
		model = t.Model
	}
	models := v.endpoints[at].Models
	found := slices.IndexFunc(models, func(m rpc.Model) bool { return m.Model == model || m.ID == model })
	if model == "" || found < 0 {
		return false, false
	}
	return slices.Contains(models[found].InputModalities, "image"), true
}

func (v *View) Prompts(key ThreadKey) []string {
	t, ok := v.threads[key]
	if !ok {
		return nil
	}
	var out []string
	for _, turn := range t.Turns {
		for _, item := range turn.Items {
			if text, ok := t.promptText(item); ok {
				out = append(out, text)
			}
		}
	}
	return out
}

func (t *ThreadView) promptText(item ItemView) (string, bool) {
	if item.Header.Type != rpc.KindUser {
		return "", false
	}
	for _, ref := range item.Header.Contents {
		page, ok := t.contents[ContentKey{Item: item.Key, ContentID: ref.ContentID}]
		if ref.Field == "text" && ok && page.Err == nil && !page.Loading && page.Next == page.Total {
			return string(page.Data), true
		}
	}
	return "", false
}

func (v *View) PendingDecisions() []Decision {
	var out []Decision
	for _, key := range v.order {
		for _, r := range v.threads[key].Requests {
			out = append(out, Decision{Request: r.Key, Method: r.Method, Answered: r.Answered})
		}
	}
	return out
}

func (m *machine) view() *View {
	v := &View{
		lanes:     map[client.EndpointID]LaneView{},
		threads:   map[ThreadKey]*ThreadView{},
		drafts:    maps.Clone(m.drafts),
		followUps: maps.Clone(m.queues),
		notices:   slices.Clone(m.notices),
		started:   maps.Clone(m.started),
		unsaved:   maps.Clone(m.unsaved),
		calls:     map[CallID]CallView{},
		keywords:  map[DraftKey]DraftKeywords{},
	}
	for _, id := range slices.Sorted(maps.Keys(m.endpoints)) {
		ep := m.endpoints[id]
		v.endpoints = append(v.endpoints, endpointView(ep))
		v.lanes[id] = ep.lane.view()
		for _, ts := range m.sortedThreads(ep) {
			v.threads[ts.key] = m.threadView(ep, ts)
			v.order = append(v.order, ts.key)
		}
	}
	for _, id := range slices.Sorted(maps.Keys(m.windows)) {
		v.windows = append(v.windows, WindowView{ID: id, Threads: slices.Clone(m.windows[id])})
	}
	for _, mu := range m.mutations {
		v.mutations = append(v.mutations, MutationView{
			OperationID: mu.id, Key: mu.key, Method: mu.method, State: mu.state, Payload: mu.payload,
			Err: mu.err, Progress: mu.progress, Receipt: mu.receipt,
		})
	}
	for id, s := range m.slots {
		v.calls[id] = s.view
	}
	m.composerViews(v)
	return v
}

func (m *machine) composerViews(v *View) {
	v.searches, v.shells = map[DraftKey]FileSearch{}, map[DraftKey][]ShellRun{}
	for key, s := range m.searches {
		v.searches[key] = s.view
	}
	for key, runs := range m.shells {
		for _, run := range runs {
			v.shells[key] = append(v.shells[key], run.view)
		}
	}
	for key, k := range m.keywords {
		v.keywords[key] = k.result
	}
}

func endpointView(ep *endpointState) EndpointView {
	return EndpointView{
		ID: ep.id, Label: ep.label, Kind: ep.kind, Phase: ep.phase, Link: ep.link, Live: ep.live, Offline: ep.lost, Err: ep.err,
		Capabilities: ep.caps, Threads: len(ep.threads), Models: ep.shared.models, Config: ep.shared.config,
		Settings: ep.shared.settings, Approval: ep.shared.text("approval"), Verbosity: ep.shared.text("verbosity"),
		StatusHide: ep.shared.list("statusHide"), SettingsErr: ep.shared.err,
	}
}

func (m *machine) threadView(ep *endpointState, ts *threadState) *ThreadView {
	if c, ok := m.cache[ts.key]; ok && c.version == ts.version && c.endpoint == ep.version {
		return c.view
	}
	live := ep.live && ts.cut.phase == cutKnown && ts.proj != nil && ts.proj.stream == ep.stream
	t := &ThreadView{
		Key: ts.key, Phase: ep.phase, Live: live, Offline: ep.lost, Opening: ts.opening != 0,
		Missing: ts.missing, Err: ts.err, LoadingOlder: ts.paging != 0,
	}
	t.contents, t.images = ts.pages.views()
	m.describe(ep, ts, t)
	if p := ts.proj; p != nil {
		if live {
			t.Link = ep.link
		}
		t.HistoryID, t.Thread, t.ActiveTurn, t.Usage, t.Cost = p.historyID, p.thread, p.activeTurn, p.usage, costOf(p.usage)
		t.HistoryComplete, t.Cut, t.Applied = p.complete, p.cut, p.applied
		t.Turns = turnViews(p)
		t.Notes, t.Children, t.Sandbox, t.Warning = p.notes, childViews(ts.key, p.children), p.sandbox, p.warning
		t.Requests = requestViews(ep, ts, live)
		t.Resolved = resolvedViews(ts.key.Endpoint, p.resolved)
	}
	m.cache[ts.key] = cachedThread{version: ts.version, endpoint: ep.version, view: t}
	return t
}

func (m *machine) describe(ep *endpointState, ts *threadState, t *ThreadView) {
	meta := m.meta[ts.key]
	model := ""
	if ts.proj != nil {
		model = ts.proj.thread.Model
	}
	t.Model = firstOf(meta.model, model, ep.shared.config.Model)
	t.Approval = firstOf(meta.approval, ep.shared.text("approval"), ep.shared.config.ApprovalPolicy)
	t.Effort = firstOf(meta.effort, ep.shared.config.ModelReasoningEffort)
}

func firstOf(values ...string) string {
	at := slices.IndexFunc(values, func(s string) bool { return s != "" })
	if at < 0 {
		return ""
	}
	return values[at]
}

func costOf(usage *rpc.ThreadTokenUsage) *Cost {
	if usage == nil || usage.CostUsdMicros == nil {
		return nil
	}
	return &Cost{USDMicros: *usage.CostUsdMicros, Partial: usage.CostPartial}
}

func turnViews(p *projection) []TurnView {
	out := make([]TurnView, 0, len(p.turns))
	for _, t := range p.turns {
		items := make([]ItemView, 0, len(t.items))
		for _, it := range t.items {
			items = append(items, ItemView{
				Key: ItemKey{Turn: t.id, Item: it.id}, Header: it.header(), Field: it.field,
				Tail: it.tail, Provisional: !it.completed && (it.started || len(it.tail) > 0), Changes: it.changes, Review: it.review,
			})
		}
		out = append(out, TurnView{
			ID: t.id, Status: t.meta.Status, Error: t.meta.Error, RetryError: t.retry, RetryWait: t.waitFor, RetryWaitAt: t.waitAt,
			StartedAt: t.meta.StartedAt, CompletedAt: t.meta.CompletedAt, DurationMs: t.meta.DurationMs,
			Paged: t.paged, ItemsComplete: t.complete, Items: items, Plan: t.plan, Effort: t.effort, Review: t.review,
		})
	}
	return out
}

func requestViews(ep *endpointState, ts *threadState, live bool) []RequestView {
	out := make([]RequestView, 0, len(ts.proj.requests))
	for _, r := range ts.proj.requests {
		to, waiting := ep.responders[r.id]
		answered := (r.answered || waiting && to.answered) && r.warning == nil
		out = append(out, RequestView{
			Key: RequestKey{Thread: ThreadKey{Endpoint: ep.id, Thread: r.owner(ts.key.Thread)}, Request: r.id}, Method: r.method, Details: r.details,
			Params: r.params, Loading: r.loading, Err: r.err, Answered: answered,
			Actionable: live && waiting && !answered && r.params != nil, Warning: r.warning, Arrival: r.arrival,
		})
	}
	return out
}

func resolvedViews(endpoint client.EndpointID, notes []resolvedNote) []ResolvedRequest {
	out := make([]ResolvedRequest, 0, len(notes))
	for _, n := range notes {
		out = append(out, ResolvedRequest{
			Key: RequestKey{Thread: ThreadKey{Endpoint: endpoint, Thread: n.thread}, Request: n.id}, Method: n.method,
			Item: n.item, Resolution: n.resolution, AnsweredHere: n.answeredHere,
		})
	}
	return out
}

func childViews(parent ThreadKey, children []childNote) []Child {
	out := make([]Child, 0, len(children))
	for _, c := range children {
		out = append(out, Child{Thread: ThreadKey{Endpoint: parent.Endpoint, Thread: c.ID}, Cwd: c.Cwd})
	}
	return out
}
