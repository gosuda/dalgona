//go:build gui

package manage

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"image"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"gioui.org/f32"
	"gioui.org/io/event"
	"gioui.org/io/input"
	"gioui.org/io/key"
	"gioui.org/io/pointer"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/text"
	"gioui.org/unit"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	localEndpoint  client.EndpointID = "/home/test/.dalgona"
	remoteEndpoint client.EndpointID = "builder"
)

type call struct {
	link      client.Link
	method    string
	params    jsontext.Value
	operation string
}

type handler func(c call) (any, error)

type fakeDaemon struct {
	t         *testing.T
	mu        sync.Mutex
	owner     *state.Owner
	caps      rpc.Capabilities
	statuses  []client.EndpointStatus
	gens      map[client.EndpointID]client.Generation
	handlers  map[string]handler
	scoped    map[client.EndpointID]map[string]handler
	gates     map[string]chan struct{}
	calls     []call
	snapshots map[state.ThreadKey]rpc.HistorySnapshot
}

var allFeatures = map[string]int{
	featureRunnerManagement: 1, featureMCPConfig: 1, featureAuth: 1, featureWorkspace: 1, featureTerminal: 1,
	featureDiagnostics: 1, featureFailures: 1, featureReporting: 1, featureReceipts: 1, "history": 1, "orderedEvents": 1,
}

func newFakeDaemon(t *testing.T) *fakeDaemon {
	return &fakeDaemon{
		t: t,
		caps: rpc.Capabilities{
			Version: 1, Features: allFeatures,
			ReceiptMethods: []string{methodThreadName, methodThreadArchive, methodThreadUnarchive, methodRemotePull, methodRemoteGC},
		},
		statuses: []client.EndpointStatus{
			{ID: localEndpoint, Label: "local", Kind: client.EndpointLocal, Phase: client.PhaseIdle},
			{ID: remoteEndpoint, Label: "builder", Kind: client.EndpointDaemon, Phase: client.PhaseIdle},
		},
		gens: map[client.EndpointID]client.Generation{}, handlers: map[string]handler{}, scoped: map[client.EndpointID]map[string]handler{},
		gates: map[string]chan struct{}{}, snapshots: map[state.ThreadKey]rpc.HistorySnapshot{},
	}
}

func stream(link client.Link) string { return fmt.Sprintf("%s-%d", link.Endpoint, link.Generation) }

func (d *fakeDaemon) Endpoints() []client.EndpointStatus {
	d.mu.Lock()
	defer d.mu.Unlock()
	return append([]client.EndpointStatus(nil), d.statuses...)
}

func (d *fakeDaemon) Connect(id client.EndpointID) error {
	d.mu.Lock()
	d.gens[id]++
	link := client.Link{Endpoint: id, Generation: d.gens[id]}
	caps := d.caps
	d.mu.Unlock()
	d.owner.PhaseChanged(client.PhaseChange{Link: link, Phase: client.PhaseConnected, Native: rpc.NativeSession{StreamID: stream(link)}, Capabilities: caps})
	return nil
}

func (d *fakeDaemon) Disconnect(id client.EndpointID) error {
	d.mu.Lock()
	link := client.Link{Endpoint: id, Generation: d.gens[id]}
	d.mu.Unlock()
	d.owner.PhaseChanged(client.PhaseChange{Link: link, Phase: client.PhaseOffline})
	return nil
}

func (d *fakeDaemon) Attach(client.EndpointID) (func(), error) { return func() {}, nil }

func (d *fakeDaemon) Respond(context.Context, client.Link, string, any) error { return nil }

func (d *fakeDaemon) Call(ctx context.Context, link client.Link, method string, params, result any) error {
	return d.dispatch(ctx, call{link: link, method: method}, params, result)
}

func (d *fakeDaemon) CallOperation(ctx context.Context, link client.Link, op rpc.Operation, params, result any) error {
	return d.dispatch(ctx, call{link: link, method: op.Method, operation: op.OperationID}, params, result)
}

func (d *fakeDaemon) on(method string, h handler) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.handlers[method] = h
}

func (d *fakeDaemon) onAt(endpoint client.EndpointID, method string, h handler) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.scoped[endpoint] == nil {
		d.scoped[endpoint] = map[string]handler{}
	}
	d.scoped[endpoint][method] = h
}

func (d *fakeDaemon) gate(method string) chan struct{} {
	d.mu.Lock()
	defer d.mu.Unlock()
	g := make(chan struct{})
	d.gates[method] = g
	return g
}

func (d *fakeDaemon) dispatch(ctx context.Context, c call, params, result any) error {
	raw, err := json.Marshal(params)
	if err != nil {
		return err
	}
	c.params = raw
	h, g := d.route(c)
	if gateErr := d.awaitGate(ctx, c, g); gateErr != nil {
		return gateErr
	}
	if h == nil {
		h = d.builtin(c)
	}
	value, err := h(c)
	if err != nil || result == nil || value == nil {
		return err
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return err
	}
	return json.Unmarshal(encoded, result)
}

func (d *fakeDaemon) route(c call) (handler, chan struct{}) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.calls = append(d.calls, c)
	h := d.scoped[c.link.Endpoint][c.method]
	if h == nil {
		h = d.handlers[c.method]
	}
	return h, d.gates[c.method]
}

func (d *fakeDaemon) awaitGate(ctx context.Context, c call, g chan struct{}) error {
	if g == nil {
		return nil
	}
	select {
	case <-g:
		return nil
	case <-ctx.Done():
		return &rpc.UnknownOutcomeError{Method: c.method, Endpoint: string(c.link.Endpoint), OperationID: c.operation, Cause: ctx.Err()}
	}
}

func (d *fakeDaemon) builtin(c call) handler {
	if c.method != "dalgona/history/open" {
		return func(call) (any, error) { return nil, rpc.NativeKind(rpc.ErrorUnavailable) }
	}
	return func(c call) (any, error) {
		var p struct {
			ThreadID string `json:"threadId"`
		}
		_ = json.Unmarshal(c.params, &p)
		d.mu.Lock()
		snap, ok := d.snapshots[state.ThreadKey{Endpoint: c.link.Endpoint, Thread: p.ThreadID}]
		d.mu.Unlock()
		if !ok {
			return nil, rpc.NativeKind(rpc.ErrorNotFound)
		}
		snap.StreamID = stream(c.link)
		return snap, nil
	}
}

func (d *fakeDaemon) sent(method string) []call {
	d.mu.Lock()
	defer d.mu.Unlock()
	var out []call
	for _, c := range d.calls {
		if c.method == method {
			out = append(out, c)
		}
	}
	return out
}

func (d *fakeDaemon) thread(key state.ThreadKey, name, active, cwd string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.snapshots[key] = rpc.HistorySnapshot{
		HistoryID: "h-" + key.Thread, CutOrdinal: "0", ActiveTurnID: active, ExpiresAt: "2099-01-01T00:00:00.000Z",
		Thread: rpc.Thread{ID: key.Thread, Name: name, Cwd: cwd, Status: rpc.ThreadStatus{Type: "idle"}},
	}
}

type rig struct {
	t         *testing.T
	daemon    *fakeDaemon
	owner     *state.Owner
	hub       *Hub
	panels    *Panels
	router    *input.Router
	shaper    *text.Shaper
	now       time.Time
	thread    state.ThreadKey
	closed    int
	opened    []shell.Route
	announced []platform.Announcement
	submitted []state.Intent
	nodes     []input.SemanticNode
	life      *fakeLifecycle
	chosen    string
	clipboard platform.ClipboardReader
}

func newRig(t *testing.T) *rig {
	t.Helper()
	shaper, err := platform.NewShaper()
	if err != nil {
		t.Fatal(err)
	}
	d := newFakeDaemon(t)
	owner := state.New(func() {}, t.TempDir())
	d.owner = owner
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		defer close(done)
		_ = owner.Run(ctx, d)
	}()
	r := &rig{t: t, daemon: d, owner: owner, router: new(input.Router), shaper: shaper, now: time.Unix(1_800_000_000, 0), life: newFakeLifecycle(d)}
	r.hub = NewHub(ctx, Deps{Session: d, Lifecycle: r.life, Invalidate: func() {}, Choose: r.choose, Browse: func(string) error { return nil }})
	r.panels = New(r.hub)
	t.Cleanup(func() {
		cancel()
		<-done
	})
	return r
}

func (r *rig) choose(_ context.Context, req platform.DialogRequest) (platform.DialogResult, error) {
	if r.chosen == "" {
		return platform.DialogResult{Cancelled: true}, nil
	}
	return platform.DialogResult{Files: []string{r.chosen}}, nil
}

func (r *rig) connect(ids ...client.EndpointID) {
	r.t.Helper()
	for _, id := range ids {
		if err := r.daemon.Connect(id); err != nil {
			r.t.Fatal(err)
		}
		r.await(func(v *state.View) bool { b := bind(v, id); return b.live })
	}
}

func (r *rig) open(key state.ThreadKey) {
	r.t.Helper()
	r.owner.Submit(state.OpenView{Window: "w", Thread: key})
	r.await(func(v *state.View) bool { tv, ok := v.Thread(key); return ok && tv.HistoryID != "" })
}

func (r *rig) await(ok func(*state.View) bool) {
	r.t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for !ok(r.owner.View()) {
		if time.Now().After(deadline) {
			r.t.Fatal("the state owner never reached the expected view")
		}
		time.Sleep(time.Millisecond)
	}
}

func (r *rig) env() shell.Env {
	return shell.Env{
		Theme: theme.Night, Shaper: r.shaper, View: r.owner.View(), Window: "w", Thread: r.thread, Modal: true,
		Submit:    func(i state.Intent) { r.submitted = append(r.submitted, i); r.owner.Submit(i) },
		Navigate:  func(state.ThreadKey) {},
		Open:      func(route shell.Route) { r.opened = append(r.opened, route) },
		Close:     func() { r.closed++ },
		Announce:  func(a platform.Announcement) { r.announced = append(r.announced, a) },
		Clipboard: func() platform.ClipboardReader { return r.clipboard },
	}
}

func (r *rig) frame(p shell.Panel) []input.SemanticNode {
	r.t.Helper()
	r.now = r.now.Add(16 * time.Millisecond)
	ops := new(op.Ops)
	gtx := layout.Context{
		Ops: ops, Now: r.now, Metric: unit.Metric{PxPerDp: 1, PxPerSp: 1},
		Constraints: layout.Exact(image.Pt(1100, 2400)), Source: r.router.Source(),
	}
	p.Layout(gtx, r.env())
	r.router.Frame(ops)
	r.nodes = r.router.AppendSemantics(nil)
	return r.nodes
}

func (r *rig) settle(p shell.Panel, w *tasks) []input.SemanticNode {
	r.t.Helper()
	w.running.Wait()
	r.frame(p)
	w.running.Wait()
	return r.frame(p)
}

func (r *rig) find(label string) (input.SemanticNode, bool) {
	for _, n := range r.nodes {
		if n.Desc.Label == label {
			return n, true
		}
	}
	return input.SemanticNode{}, false
}

func (r *rig) button(label string) input.SemanticNode {
	r.t.Helper()
	for _, n := range r.nodes {
		if (n.Desc.Class == semantic.Button || n.Desc.Class == semantic.CheckBox) && n.Desc.Label == label {
			return n
		}
	}
	r.t.Fatalf("no button %q among %s", label, r.labels())
	return input.SemanticNode{}
}

func (r *rig) labels() string {
	var b strings.Builder
	for _, n := range r.nodes {
		if n.Desc.Label != "" {
			fmt.Fprintf(&b, "%q ", n.Desc.Label)
		}
	}
	return b.String()
}

func (r *rig) press(bounds image.Rectangle) {
	at := f32.Pt(float32(bounds.Min.X+bounds.Max.X)/2, float32(bounds.Min.Y+bounds.Max.Y)/2)
	r.router.Queue(
		pointer.Event{Kind: pointer.Press, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at, Time: time.Second},
		pointer.Event{Kind: pointer.Release, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at, Time: time.Second + 50*time.Millisecond},
	)
}

func (r *rig) click(p shell.Panel, label string) []input.SemanticNode {
	r.t.Helper()
	r.press(r.button(label).Desc.Bounds)
	return r.frame(p)
}

func (r *rig) editor(label string) input.SemanticNode {
	r.t.Helper()
	for _, parent := range r.nodes {
		if parent.Desc.Label != label {
			continue
		}
		for _, n := range r.nodes {
			if n.ParentID == parent.ID && n.Desc.Class == semantic.Editor {
				return n
			}
		}
	}
	r.t.Fatalf("no field %q among %s", label, r.labels())
	return input.SemanticNode{}
}

func (r *rig) typeInto(p shell.Panel, label, text string) []input.SemanticNode {
	r.t.Helper()
	r.press(r.editor(label).Desc.Bounds)
	r.frame(p)
	r.router.Queue(key.EditEvent{Range: key.Range{Start: 0, End: 1 << 20}, Text: text})
	r.frame(p)
	return r.frame(p)
}

func (r *rig) shows(fragment string) bool {
	for _, n := range r.nodes {
		if strings.Contains(n.Desc.Label, fragment) {
			return true
		}
	}
	return false
}

func (r *rig) mustShow(fragment string) {
	r.t.Helper()
	if !r.shows(fragment) {
		r.t.Fatalf("no label contains %q among %s", fragment, r.labels())
	}
}

func (r *rig) mustHide(fragment string) {
	r.t.Helper()
	if r.shows(fragment) {
		r.t.Fatalf("a label contains %q, which must not show", fragment)
	}
}

func params[T any](t *testing.T, c call) T {
	t.Helper()
	var out T
	if err := json.Unmarshal(c.params, &out); err != nil {
		t.Fatalf("params of %s: %v", c.method, err)
	}
	return out
}

func decodeParams(c call, out any) error {
	if err := json.Unmarshal(c.params, out); err != nil {
		return rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return nil
}

func (r *rig) focus(p shell.Panel, tag event.Tag) {
	r.t.Helper()
	gtx := layout.Context{Ops: new(op.Ops), Source: r.router.Source()}
	gtx.Execute(key.FocusCmd{Tag: tag})
	r.frame(p)
}

func (r *rig) focused(tag event.Tag) bool { return r.router.Source().Focused(tag) }

func (r *rig) key(p shell.Panel, name key.Name, mods key.Modifiers) {
	r.t.Helper()
	r.router.Queue(key.Event{Name: name, Modifiers: mods, State: key.Press}, key.Event{Name: name, Modifiers: mods, State: key.Release})
	r.frame(p)
}

func eventually(t *testing.T, what string, ok func() bool) {
	t.Helper()
	for range 5000 {
		if ok() {
			return
		}
		sleepBriefly()
	}
	t.Fatalf("timed out waiting until %s", what)
}

var errTransportLost = errors.New("transport closed after dispatch")

type fakeLifecycle struct {
	daemon *fakeDaemon
	mu     sync.Mutex
	specs  map[string]client.RemoteSpec
	saved  []string
	saveFn func(name string, spec client.RemoteSpec) error
	disc   []client.EndpointID
	ups    []client.CloudUpRequest
	pulls  []client.CloudPullRequest
	downs  []client.CloudDownRequest
	upGate chan struct{}
	diag   []DiagnosticRow
	diags  []bool
}

func newFakeLifecycle(d *fakeDaemon) *fakeLifecycle {
	return &fakeLifecycle{daemon: d, specs: map[string]client.RemoteSpec{string(remoteEndpoint): fixtureRemoteSpec()}}
}

func fixtureRemoteSpec() client.RemoteSpec {
	return client.RemoteSpec{Kind: client.RemoteDaemon, URL: "wss://builder.example/rpc", TokenFile: filepath.Join("/home", "test", ".dalgona", "builder.token")}
}

func (l *fakeLifecycle) Endpoints() []client.EndpointStatus { return l.daemon.Endpoints() }

func (l *fakeLifecycle) Remote(name string) (client.RemoteSpec, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	spec, ok := l.specs[name]
	if !ok {
		return client.RemoteSpec{}, fmt.Errorf("remote %q is not configured", name)
	}
	return spec, nil
}

func (l *fakeLifecycle) SaveRemote(name string, spec client.RemoteSpec) error {
	if l.saveFn != nil {
		if err := l.saveFn(name, spec); err != nil {
			return err
		}
	}
	l.mu.Lock()
	l.specs[name] = spec
	l.saved = append(l.saved, name)
	l.mu.Unlock()
	l.daemon.mu.Lock()
	defer l.daemon.mu.Unlock()
	for _, s := range l.daemon.statuses {
		if s.ID == client.EndpointID(name) {
			return nil
		}
	}
	kind := client.EndpointBox
	if spec.Kind == client.RemoteDaemon {
		kind = client.EndpointDaemon
	}
	l.daemon.statuses = append(l.daemon.statuses, client.EndpointStatus{ID: client.EndpointID(name), Label: name, Kind: kind})
	return nil
}

func (l *fakeLifecycle) Disconnect(id client.EndpointID) error {
	l.mu.Lock()
	l.disc = append(l.disc, id)
	l.mu.Unlock()
	return l.daemon.Disconnect(id)
}

func (l *fakeLifecycle) CloudUp(ctx context.Context, req client.CloudUpRequest) (client.CloudBox, error) {
	l.mu.Lock()
	l.ups = append(l.ups, req)
	g := l.upGate
	l.mu.Unlock()
	req.Progress(client.Progress{Endpoint: req.Endpoint, OperationID: req.OperationID, Phase: client.ProgressConnecting, Done: 0, Total: 2})
	if g != nil {
		select {
		case <-g:
		case <-ctx.Done():
			return client.CloudBox{}, ctx.Err()
		}
	}
	return client.CloudBox{Service: "e2b", BoxID: "box-9"}, nil
}

func (l *fakeLifecycle) CloudPull(_ context.Context, req client.CloudPullRequest) (bool, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.pulls = append(l.pulls, req)
	return true, nil
}

func (l *fakeLifecycle) CloudDown(_ context.Context, req client.CloudDownRequest) error {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.downs = append(l.downs, req)
	return nil
}

func (l *fakeLifecycle) Diagnose(_ context.Context, endpoint client.EndpointID, online bool) ([]DiagnosticRow, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.diags = append(l.diags, online)
	if endpoint != localEndpoint {
		return nil, fmt.Errorf("local diagnostics describe only %s", localEndpoint)
	}
	return l.diag, nil
}

func sleepBriefly() { time.Sleep(time.Millisecond) }
