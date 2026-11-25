//go:build gui

package manage

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"os"
	"slices"
	"sync"
	"time"

	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	metadataDeadline = 15 * time.Second
	pageDeadline     = 35 * time.Second
	decisionDeadline = 6 * time.Minute
	longDeadline     = 30 * time.Minute
)

type Session interface {
	Call(ctx context.Context, link client.Link, method string, params, result any) error
	CallOperation(ctx context.Context, link client.Link, op rpc.Operation, params, result any) error
}

type DiagnosticRow struct {
	Status string
	Check  string
	Detail string
}

type Lifecycle interface {
	Endpoints() []client.EndpointStatus
	Remote(name string) (client.RemoteSpec, error)
	SaveRemote(name string, spec client.RemoteSpec) error
	Disconnect(id client.EndpointID) error
	CloudUp(ctx context.Context, req client.CloudUpRequest) (client.CloudBox, error)
	CloudPull(ctx context.Context, req client.CloudPullRequest) (bool, error)
	CloudDown(ctx context.Context, req client.CloudDownRequest) error
	Diagnose(ctx context.Context, endpoint client.EndpointID, online bool) ([]DiagnosticRow, error)
}

type Deps struct {
	Session    Session
	Lifecycle  Lifecycle
	Choose     func(ctx context.Context, req platform.DialogRequest) (platform.DialogResult, error)
	Browse     func(url string) error
	Invalidate func()
	Now        func() time.Time
}

type Hub struct {
	deps       Deps
	ctx        context.Context
	mu         sync.Mutex
	listeners  map[listenKey]func(jsontext.Value)
	workspaces *workspaces
}

type listenKey struct {
	endpoint client.EndpointID
	method   string
	id       string
}

func NewHub(ctx context.Context, deps Deps) *Hub {
	if deps.Now == nil {
		deps.Now = time.Now
	}
	h := &Hub{deps: deps, ctx: ctx, listeners: map[listenKey]func(jsontext.Value){}}
	h.workspaces = newWorkspaces(h)
	return h
}

func (h *Hub) Notified(link client.Link, method string, params jsontext.Value) {
	member, ok := listenMembers[method]
	if !ok {
		return
	}
	var probe map[string]jsontext.Value
	if json.Unmarshal(params, &probe) != nil {
		return
	}
	var id string
	if json.Unmarshal(probe[member], &id) != nil {
		return
	}
	h.mu.Lock()
	fn := h.listeners[listenKey{endpoint: link.Endpoint, method: method, id: id}]
	h.mu.Unlock()
	if fn != nil {
		fn(params)
	}
}

var listenMembers = map[string]string{
	methodWorkspaceChanged: "watchId",
	methodRemoteProgress:   "operationId",
}

func (h *Hub) listen(key listenKey, fn func(jsontext.Value)) func() {
	h.mu.Lock()
	h.listeners[key] = fn
	h.mu.Unlock()
	return func() {
		h.mu.Lock()
		delete(h.listeners, key)
		h.mu.Unlock()
	}
}

type tasks struct {
	mu      sync.Mutex
	done    []func()
	wake    func()
	running sync.WaitGroup
}

func (t *tasks) spawn(work func() func()) {
	t.running.Add(1)
	go func() {
		defer t.running.Done()
		apply := work()
		t.post(apply)
	}()
}

func (t *tasks) post(apply func()) {
	if apply == nil {
		return
	}
	t.mu.Lock()
	t.done = append(t.done, apply)
	t.mu.Unlock()
	if t.wake != nil {
		t.wake()
	}
}

func (t *tasks) settle() {
	t.mu.Lock()
	done := t.done
	t.done = nil
	t.mu.Unlock()
	for _, apply := range done {
		apply()
	}
}

func endpointView(v *state.View, id client.EndpointID) (state.EndpointView, bool) {
	if v == nil {
		return state.EndpointView{}, false
	}
	for _, e := range v.Endpoints() {
		if e.ID == id {
			return e, true
		}
	}
	return state.EndpointView{}, false
}

func phaseWord(p client.Phase) string {
	switch p {
	case client.PhaseIdle:
		return "Idle"
	case client.PhaseConnecting:
		return "Connecting"
	case client.PhaseConnected:
		return "Connected"
	case client.PhaseReconnecting:
		return "Reconnecting"
	case client.PhaseOffline:
		return "Offline"
	case client.PhaseIncompatible:
		return "Incompatible"
	case client.PhaseFailed:
		return "Failed"
	}
	return "Unknown"
}

type binding struct {
	endpoint client.EndpointID
	link     client.Link
	live     bool
	phase    client.Phase
	features map[string]int
	receipts []string
}

func bind(env *state.View, id client.EndpointID) binding {
	e, ok := endpointView(env, id)
	if !ok {
		return binding{endpoint: id, phase: client.PhaseOffline}
	}
	return binding{endpoint: id, link: e.Link, live: e.Live, phase: e.Phase, features: e.Capabilities.Features, receipts: e.Capabilities.ReceiptMethods}
}

func (b binding) gate(feature, operation string) string {
	if !b.live {
		return fmt.Sprintf("%s is %s; connect it to %s.", b.endpoint, phaseWord(b.phase), operation)
	}
	if b.features[feature] < 1 {
		return fmt.Sprintf("The daemon on %s does not offer %s (%s:1); upgrade that daemon to %s.", b.endpoint, operation, feature, operation)
	}
	return ""
}

func (h *Hub) call(b binding, deadline time.Duration, method string, params, result any) error {
	ctx, cancel := context.WithTimeout(h.ctx, deadline)
	defer cancel()
	return h.deps.Session.Call(ctx, b.link, method, params, result)
}

type operationRequest struct {
	binding     binding
	deadline    time.Duration
	method      string
	operationID string
	params      any
	result      any
}

func (h *Hub) callContext(ctx context.Context, req operationRequest) error {
	ctx, cancel := context.WithTimeout(ctx, req.deadline)
	defer cancel()
	return h.deps.Session.Call(ctx, req.binding.link, req.method, req.params, req.result)
}

func (h *Hub) mutate(req operationRequest) error {
	if !slices.Contains(req.binding.receipts, req.method) {
		return h.call(req.binding, req.deadline, req.method, req.params, req.result)
	}
	ctx, cancel := context.WithTimeout(h.ctx, req.deadline)
	defer cancel()
	return h.deps.Session.CallOperation(ctx, req.binding.link, rpc.Operation{Method: req.method, OperationID: req.operationID}, req.params, req.result)
}

type outcome uint8

const (
	outcomeIdle outcome = iota
	outcomeRunning
	outcomeDone
	outcomeFailed
	outcomeUnknown
)

type mutation struct {
	method    string
	id        string
	endpoint  client.EndpointID
	state     outcome
	err       error
	checking  bool
	checkable bool
	check     widget.Clickable
}

func (m *mutation) begin(method string, b binding, native bool) string {
	m.method, m.id, m.endpoint, m.state, m.err, m.checking = method, rpc.NewOperationID(), b.endpoint, outcomeRunning, nil, false
	m.checkable = b.features[featureReceipts] >= 1 && (native || slices.Contains(b.receipts, method))
	return m.id
}

func (m *mutation) running() bool { return m.state == outcomeRunning || m.checking }

func (m *mutation) settle(err error) {
	switch {
	case err == nil:
		m.state, m.err = outcomeDone, nil
	case uncertain(err):
		m.state, m.err = outcomeUnknown, err
	default:
		m.state, m.err = outcomeFailed, err
	}
}

func uncertain(err error) bool {
	if _, ok := errors.AsType[*rpc.UnknownOutcomeError](err); ok {
		return true
	}
	if data, ok := rpc.ErrorDataOf(err); ok {
		return data.Kind == rpc.ErrorUnknownOutcome
	}
	if errors.Is(err, client.ErrStaleLink) || errors.Is(err, client.ErrEndpointSetClosed) {
		return false
	}
	_, coded := errors.AsType[*rpc.RPCError](err)
	return err != nil && !coded
}

type receipt struct {
	State  string         `json:"state"`
	Method string         `json:"method"`
	Result jsontext.Value `json:"result,omitzero"`
	Error  *rpc.RPCError  `json:"error,omitzero"`
}

func (h *Hub) readReceipt(b binding, id string) (receipt, error) {
	var r receipt
	err := h.call(b, metadataDeadline, methodOperationRead, operationRead{OperationID: id}, &r)
	if data, ok := rpc.ErrorDataOf(err); ok && data.Kind == rpc.ErrorNotFound {
		return receipt{State: "absent"}, nil
	}
	return r, err
}

func (m *mutation) reconcile(r receipt, err error) jsontext.Value {
	m.checking = false
	if err != nil {
		m.err = fmt.Errorf("checking operation %s on %s failed: %w", m.id, m.endpoint, err)
		return nil
	}
	switch r.State {
	case "succeeded":
		m.state, m.err = outcomeDone, nil
		return r.Result
	case "failed":
		m.state, m.err = outcomeFailed, r.Error
	case "absent":
		m.state, m.err = outcomeFailed, fmt.Errorf("%s never reached %s; it is safe to try again", m.method, m.endpoint)
	case "reserved", "running":
		m.state, m.err = outcomeUnknown, fmt.Errorf("%s is still running on %s; check again later", m.method, m.endpoint)
	default:
		m.state, m.err = outcomeUnknown, fmt.Errorf("%s on %s has an unknown outcome the daemon cannot settle; inspect the result before repeating it", m.method, m.endpoint)
	}
	return nil
}

func (m *mutation) status() (tone, string) {
	switch m.state {
	case outcomeRunning:
		return toneDim, "Working on " + string(m.endpoint) + "…"
	case outcomeFailed:
		return toneFailed, explain(m.err)
	case outcomeUnknown:
		if !m.checkable {
			return toneWarning, "Outcome unknown on " + string(m.endpoint) + ": " + explain(m.err) + " This daemon keeps no receipts; reload to see the current state before trying again."
		}
		return toneWarning, "Outcome unknown on " + string(m.endpoint) + ": " + explain(m.err) + " Check the outcome before trying again."
	case outcomeIdle, outcomeDone:
	}
	return toneBody, ""
}

func (m *mutation) line(env scene) layout.Widget {
	t, text := m.status()
	if text == "" {
		return nil
	}
	if m.state != outcomeUnknown || !m.checkable {
		return sayer(env, t, text)
	}
	return func(gtx layout.Context) layout.Dimensions {
		return stack(gtx, theme.Space2, sayer(env, t, text), buttons(env, action{click: &m.check, label: "Check outcome", variant: secondary, off: m.checking}))
	}
}

func (h *Hub) checkOutcome(gtx layout.Context, work *tasks, v *state.View, m *mutation, done func(jsontext.Value)) {
	b := bind(v, m.endpoint)
	if !clicked(gtx, &m.check, m.checking || m.state != outcomeUnknown || !m.checkable || !b.live) {
		return
	}
	m.checking = true
	id := m.id
	work.spawn(func() func() {
		r, err := h.readReceipt(b, id)
		return func() {
			if m.id != id {
				return
			}
			result := m.reconcile(r, err)
			if m.state == outcomeDone && done != nil {
				done(result)
			}
		}
	})
}

var kindText = map[rpc.ErrorKind]string{
	rpc.ErrorInvalidParams:     "The daemon rejected the input as invalid",
	rpc.ErrorUnavailable:       "The target or a required service cannot be reached right now",
	rpc.ErrorIncompatible:      "The daemon cannot serve this request; upgrade the daemon or client",
	rpc.ErrorStaleRevision:     "The content changed since it was loaded; reload it",
	rpc.ErrorPermissionDenied:  "Permission was declined or revoked",
	rpc.ErrorLimitExceeded:     "A documented size or count limit would be exceeded",
	rpc.ErrorExpired:           "The handle or session expired; open it again",
	rpc.ErrorBusy:              "Another operation on this resource is still running; wait for it",
	rpc.ErrorUnknownOutcome:    "The request was sent but its final outcome is unknown",
	rpc.ErrorReplayGap:         "Older output is no longer retained",
	rpc.ErrorCancelled:         "The request was cancelled before any effect",
	rpc.ErrorNotFound:          "No such object exists",
	rpc.ErrorOperationConflict: "The operation identifier was reused with different input",
}

func explain(err error) string {
	if err == nil {
		return ""
	}
	if data, ok := rpc.ErrorDataOf(err); ok {
		text := kindText[data.Kind]
		if text == "" {
			text = string(data.Kind)
		}
		if data.Stage != "" {
			text += " (stage " + data.Stage + ")"
		}
		return text + "."
	}
	return err.Error()
}

func kindOf(err error) rpc.ErrorKind {
	if data, ok := rpc.ErrorDataOf(err); ok {
		return data.Kind
	}
	return ""
}

func (h *Hub) export(name string, data []byte) error {
	if h.deps.Choose == nil {
		return errors.New("this build has no native save dialog; export is unavailable")
	}
	ctx, cancel := context.WithTimeout(h.ctx, decisionDeadline)
	defer cancel()
	res, err := h.deps.Choose(ctx, platform.DialogRequest{Kind: platform.DialogSaveFile, Title: "Export " + name, SuggestedName: name})
	switch {
	case err != nil:
		return err
	case res.Cancelled || len(res.Files) == 0:
		return errExportCancelled
	}
	return os.WriteFile(res.Files[0], data, 0o600)
}

var errExportCancelled = errors.New("export cancelled; nothing was written")

func (h *Hub) chooseFolder(title string) (string, error) {
	if h.deps.Choose == nil {
		return "", errors.New("this build has no native folder dialog")
	}
	ctx, cancel := context.WithTimeout(h.ctx, decisionDeadline)
	defer cancel()
	res, err := h.deps.Choose(ctx, platform.DialogRequest{Kind: platform.DialogChooseFolder, Title: title})
	switch {
	case err != nil:
		return "", err
	case res.Cancelled || len(res.Files) == 0:
		return "", nil
	}
	return res.Files[0], nil
}
