package client

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"maps"
	"path/filepath"
	"slices"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	MaxEndpoints     = 32
	MaxLiveEndpoints = 8

	methodCapabilitiesRead = "dalgona/capabilities/read"
	featureCancellation    = "requestCancellation"
	checkpointVersion      = 1
)

var (
	ErrLiveLimit         = errors.New("client: 8 endpoints hold live connections with work in progress; disconnect one in Endpoints and retry")
	ErrStaleLink         = errors.New("the connection of this request has ended; retry after the endpoint connects")
	ErrEndpointSetClosed = errors.New("client: endpoint set closed")
)

var requiredFeatures = [...]string{
	"orderedEvents", featureCancellation, "history", "mutationReceipts", "imageRead", "threadUnsubscribe",
	"auth", "mcpConfig", "workspace", "terminal", "runnerManagement", "diagnostics", "failureRecords", "reporting",
}

type (
	EndpointID       string
	Generation       uint64
	EndpointKind     uint8
	EndpointProtocol uint8

	Link struct {
		Endpoint   EndpointID
		Generation Generation
	}

	EndpointSpec struct {
		ID    EndpointID
		Label string
		Kind  EndpointKind
		Dial  func(context.Context) (io.ReadWriteCloser, error)
	}

	PhaseChange struct {
		Link         Link
		Phase        Phase
		Native       rpc.NativeSession
		Initialized  rpc.InitializeResult
		Capabilities rpc.Capabilities
		Err          error
	}

	Observer interface {
		PhaseChanged(PhaseChange)
		Notified(link Link, method string, params jsontext.Value)
		Requested(ctx context.Context, link Link, method string, params jsontext.Value) (any, error)
	}
)

const (
	EndpointLocal EndpointKind = iota
	EndpointBox
	EndpointDaemon
	EndpointSSH
	EndpointDevice
)

const (
	ProtocolNative EndpointProtocol = iota
	ProtocolTUI
)

type EndpointSet struct {
	observer Observer
	remotes  *Remotes
	protocol EndpointProtocol
	ctx      context.Context
	cancel   context.CancelFunc
	wake     chan struct{}
	tasks    sync.WaitGroup
	saving   sync.Mutex
	mu       sync.Mutex
	order    []*endpoint
	byID     map[EndpointID]*endpoint
	notices  []phaseNotice
	uses     uint64
	closed   bool
}

type EndpointStatus struct {
	ID    EndpointID
	Label string
	Kind  EndpointKind
	Phase Phase
}

type endpoint struct {
	spec       EndpointSpec
	phase      Phase
	generation Generation
	link       *link
	stop       context.CancelFunc
	pins       int
	lastUse    uint64
}

type phaseNotice struct {
	change    PhaseChange
	announced chan struct{}
}

func ConfiguredEndpoints(cfg config.Config, backend Backend, remotes *Remotes) []EndpointSpec {
	specs := []EndpointSpec{{ID: EndpointID(filepath.Clean(cfg.Home)), Label: "local", Kind: EndpointLocal, Dial: sessionDial(Local(cfg.Home, backend))}}
	for _, name := range slices.Sorted(maps.Keys(cfg.Remotes)) {
		specs = append(specs, remoteEndpoint(name, cfg.Remotes[name], remotes))
	}
	return specs
}

func remoteEndpoint(name string, spec config.RemoteSpec, remotes *Remotes) EndpointSpec {
	kind := EndpointBox
	switch spec.Kind {
	case config.RemoteDaemon:
		kind = EndpointDaemon
	case config.RemoteSSH:
		kind = EndpointSSH
	case config.RemoteDevice:
		kind = EndpointDevice
	}
	dial := func(ctx context.Context) (io.ReadWriteCloser, error) { return remotes.dial(ctx, name) }
	return EndpointSpec{ID: EndpointID(name), Label: name, Kind: kind, Dial: dial}
}

func sessionDial(session *Session) func(context.Context) (io.ReadWriteCloser, error) {
	return func(ctx context.Context) (io.ReadWriteCloser, error) { return session.Connect(ctx) }
}

func NewEndpointSet(remotes *Remotes, specs []EndpointSpec, observer Observer, protocol EndpointProtocol) (*EndpointSet, error) {
	if protocol != ProtocolNative && protocol != ProtocolTUI {
		return nil, fmt.Errorf("client: unsupported endpoint protocol %d", protocol)
	}
	if len(specs) > MaxEndpoints {
		return nil, fmt.Errorf("client: %d endpoints are configured and the limit is %d; remove [remotes] entries", len(specs), MaxEndpoints)
	}
	ctx, cancel := context.WithCancel(context.Background())
	s := &EndpointSet{observer: observer, remotes: remotes, protocol: protocol, ctx: ctx, cancel: cancel, wake: make(chan struct{}, 1), byID: make(map[EndpointID]*endpoint, len(specs))}
	for _, spec := range specs {
		if _, repeated := s.byID[spec.ID]; repeated || spec.ID == "" {
			cancel()
			return nil, fmt.Errorf("client: endpoint id %q is empty or repeated", spec.ID)
		}
		e := &endpoint{spec: spec}
		s.byID[spec.ID] = e
		s.order = append(s.order, e)
	}
	s.tasks.Add(1)
	go s.deliver()
	return s, nil
}

func (s *EndpointSet) Endpoints() []EndpointStatus {
	s.mu.Lock()
	defer s.mu.Unlock()
	statuses := make([]EndpointStatus, len(s.order))
	for i, e := range s.order {
		statuses[i] = EndpointStatus{ID: e.spec.ID, Label: e.spec.Label, Kind: e.spec.Kind, Phase: e.phase}
	}
	return statuses
}

func (s *EndpointSet) SaveRemote(name string, spec config.RemoteSpec) error {
	s.saving.Lock()
	defer s.saving.Unlock()
	if err := s.admit(EndpointID(name)); err != nil {
		return err
	}
	if err := s.remotes.SaveRemote(name, spec); err != nil {
		return err
	}
	return s.recordRemote(name, spec)
}

func (s *EndpointSet) recordRemote(name string, spec config.RemoteSpec) error {
	saved := remoteEndpoint(name, spec, s.remotes)
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return ErrEndpointSetClosed
	}
	if e, ok := s.byID[saved.ID]; ok {
		e.spec.Kind = saved.Kind
		return nil
	}
	e := &endpoint{spec: saved}
	s.byID[saved.ID] = e
	s.order = append(s.order, e)
	return nil
}

func (s *EndpointSet) admit(id EndpointID) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return ErrEndpointSetClosed
	}
	if _, ok := s.byID[id]; !ok && len(s.order) >= MaxEndpoints {
		return fmt.Errorf("client: %d endpoints are configured and the limit is %d; remove a remote before adding %s", len(s.order), MaxEndpoints, id)
	}
	return nil
}

func (s *EndpointSet) Connect(id EndpointID) error {
	s.mu.Lock()
	e, err := s.lookupLocked(id)
	if err != nil || e.phase.live() {
		s.mu.Unlock()
		return err
	}
	evicted, err := s.reserveLocked()
	if err == nil {
		s.startLocked(e, PhaseConnecting, nil, 0, retryLadder[:])
	}
	s.mu.Unlock()
	return errors.Join(err, evicted.close())
}

func (s *EndpointSet) Disconnect(id EndpointID) error {
	s.mu.Lock()
	e, err := s.lookupLocked(id)
	if err != nil || e.phase == PhaseOffline {
		s.mu.Unlock()
		return err
	}
	if e.stop != nil {
		e.stop()
	}
	dropped := e.link
	e.link = nil
	s.setPhaseLocked(e, PhaseOffline, nil)
	s.mu.Unlock()
	return dropped.close()
}

func (s *EndpointSet) Attach(id EndpointID) (func(), error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	e, err := s.lookupLocked(id)
	if err != nil {
		return nil, err
	}
	e.pins++
	return s.unpin(e), nil
}

func (s *EndpointSet) Call(ctx context.Context, link Link, method string, params, result any) error {
	conn, release, err := s.use(link)
	if err != nil {
		return err
	}
	defer release()
	return conn.Call(ctx, method, params, result)
}

func (s *EndpointSet) CallOperation(ctx context.Context, link Link, op rpc.Operation, params, result any) error {
	if s.protocol != ProtocolNative {
		return fmt.Errorf("client: operation receipts require the native protocol")
	}
	conn, release, err := s.use(link)
	if err != nil {
		return err
	}
	defer release()
	op.Endpoint, op.Generation = string(link.Endpoint), uint64(link.Generation)
	return conn.CallOperation(ctx, op, params, result)
}

func (s *EndpointSet) Respond(_ context.Context, link Link, requestID string, result any) error {
	s.mu.Lock()
	e, err := s.lookupLocked(link.Endpoint)
	if err != nil {
		s.mu.Unlock()
		return err
	}
	if e.link == nil || e.link.route.link != link {
		s.mu.Unlock()
		return fmt.Errorf("endpoint %s is %s: %w", e.spec.Label, e.phase, ErrStaleLink)
	}
	route := e.link.route
	s.mu.Unlock()
	return route.respond(requestID, result)
}

func (s *EndpointSet) Close() error {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return nil
	}
	s.closed = true
	s.cancel()
	var dropped []*link
	for _, e := range s.order {
		if e.link != nil {
			dropped = append(dropped, e.link)
			e.link = nil
		}
	}
	s.signalLocked()
	s.mu.Unlock()
	var errs []error
	for _, l := range dropped {
		errs = append(errs, l.close())
	}
	s.tasks.Wait()
	return errors.Join(errs...)
}

func (s *EndpointSet) lookupLocked(id EndpointID) (*endpoint, error) {
	if s.closed {
		return nil, ErrEndpointSetClosed
	}
	e, ok := s.byID[id]
	if !ok {
		return nil, fmt.Errorf("client: endpoint %q is not configured", id)
	}
	return e, nil
}

func (s *EndpointSet) use(link Link) (*rpc.Conn, func(), error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	e, err := s.lookupLocked(link.Endpoint)
	if err != nil {
		return nil, nil, err
	}
	if e.link == nil || e.link.route.link != link {
		return nil, nil, fmt.Errorf("endpoint %s is %s: %w", e.spec.Label, e.phase, ErrStaleLink)
	}
	e.pins++
	e.lastUse = s.tick()
	return e.link.conn, s.unpin(e), nil
}

func (s *EndpointSet) unpin(e *endpoint) func() {
	return sync.OnceFunc(func() {
		s.mu.Lock()
		e.pins--
		e.lastUse = s.tick()
		s.mu.Unlock()
	})
}

func (s *EndpointSet) tick() uint64 {
	s.uses++
	return s.uses
}

func (s *EndpointSet) reserveLocked() (*link, error) {
	if s.liveLocked() < MaxLiveEndpoints {
		return nil, nil
	}
	victim := s.leastRecentIdleLocked()
	if victim == nil {
		return nil, ErrLiveLimit
	}
	evicted := victim.link
	victim.link = nil
	s.setPhaseLocked(victim, PhaseIdle, nil)
	return evicted, nil
}

func (s *EndpointSet) liveLocked() int {
	live := 0
	for _, e := range s.order {
		if e.phase.live() {
			live++
		}
	}
	return live
}

func (s *EndpointSet) leastRecentIdleLocked() *endpoint {
	var victim *endpoint
	for _, e := range s.order {
		idle := e.phase == PhaseConnected && e.pins == 0
		if idle && (victim == nil || e.lastUse < victim.lastUse) {
			victim = e
		}
	}
	return victim
}

func (s *EndpointSet) startLocked(e *endpoint, phase Phase, cause error, delay time.Duration, ladder []time.Duration) {
	ctx, stop := context.WithCancel(s.ctx)
	e.stop = stop
	s.setPhaseLocked(e, phase, cause)
	s.tasks.Add(1)
	go s.campaign(ctx, stop, e, delay, ladder)
}

func (s *EndpointSet) campaign(ctx context.Context, stop context.CancelFunc, e *endpoint, delay time.Duration, ladder []time.Duration) {
	defer s.tasks.Done()
	defer stop()
	for sleep(ctx, delay) && s.attempt(ctx, e, len(ladder)) {
		delay, ladder = ladder[0], ladder[1:]
	}
}

func (s *EndpointSet) attempt(ctx context.Context, e *endpoint, retriesLeft int) bool {
	l, ok := s.prepare(ctx, e)
	if !ok {
		return false
	}
	err := l.open(ctx, e.spec.Dial, s.protocol)
	retry, discard := s.settle(ctx, e, l, err, retriesLeft)
	if discard {
		_ = l.close()
	}
	return retry
}

func (s *EndpointSet) prepare(ctx context.Context, e *endpoint) (*link, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if ctx.Err() != nil {
		return nil, false
	}
	e.generation++
	routeCtx, cancel := context.WithCancel(s.ctx)
	route := &linkRoute{link: Link{Endpoint: e.spec.ID, Generation: e.generation}, observer: s.observer, ctx: routeCtx, announced: make(chan struct{})}
	return &link{route: route, cancel: cancel}, true
}

func (s *EndpointSet) settle(ctx context.Context, e *endpoint, l *link, err error, retriesLeft int) (retry, discard bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	switch {
	case ctx.Err() != nil:
		return false, err == nil
	case err == nil:
		s.connectLocked(e, l)
		return false, false
	}
	phase := failurePhase(err)
	retry = phase == PhaseOffline && retriesLeft > 0
	if retry {
		phase = PhaseReconnecting
	}
	s.setPhaseLocked(e, phase, err)
	return retry, false
}

func (s *EndpointSet) connectLocked(e *endpoint, l *link) {
	e.link = l
	e.phase = PhaseConnected
	e.lastUse = s.tick()
	s.noticeLocked(phaseNotice{change: PhaseChange{Link: l.route.link, Phase: PhaseConnected, Native: l.native, Initialized: l.initialized, Capabilities: l.caps}, announced: l.route.announced})
	s.tasks.Add(1)
	go s.watch(e, l)
}

func (s *EndpointSet) watch(e *endpoint, l *link) {
	defer s.tasks.Done()
	<-l.conn.Done()
	l.cancel()
	s.mu.Lock()
	defer s.mu.Unlock()
	if e.link != l || s.closed {
		return
	}
	e.link = nil
	s.startLocked(e, PhaseReconnecting, errTransportLost, retryLadder[0], retryLadder[1:])
}

func (s *EndpointSet) setPhaseLocked(e *endpoint, phase Phase, cause error) {
	e.phase = phase
	s.noticeLocked(phaseNotice{change: PhaseChange{Link: Link{Endpoint: e.spec.ID, Generation: e.generation}, Phase: phase, Err: cause}})
}

func (s *EndpointSet) noticeLocked(notice phaseNotice) {
	s.notices = append(s.notices, notice)
	s.signalLocked()
}

func (s *EndpointSet) signalLocked() {
	select {
	case s.wake <- struct{}{}:
	default:
	}
}

func (s *EndpointSet) deliver() {
	defer s.tasks.Done()
	for {
		notice, ok := s.nextNotice()
		if !ok {
			return
		}
		s.observer.PhaseChanged(notice.change)
		if notice.announced != nil {
			close(notice.announced)
		}
	}
}

func (s *EndpointSet) nextNotice() (phaseNotice, bool) {
	for {
		s.mu.Lock()
		switch {
		case s.closed:
			s.mu.Unlock()
			return phaseNotice{}, false
		case len(s.notices) > 0:
			notice := s.notices[0]
			s.notices = s.notices[1:]
			s.mu.Unlock()
			return notice, true
		}
		s.mu.Unlock()
		<-s.wake
	}
}

type link struct {
	route       *linkRoute
	cancel      context.CancelFunc
	conn        *rpc.Conn
	native      rpc.NativeSession
	initialized rpc.InitializeResult
	caps        rpc.Capabilities
}

func (l *link) open(ctx context.Context, dial func(context.Context) (io.ReadWriteCloser, error), protocol EndpointProtocol) error {
	ctx, cancel := context.WithTimeout(ctx, attemptDeadline)
	defer cancel()
	rwc, err := dial(ctx)
	if err != nil {
		l.cancel()
		return err
	}
	l.conn = rpc.Dial(l.route.ctx, rwc, l.route)
	if err = l.initialize(ctx, protocol); err != nil {
		_ = l.close()
		return err
	}
	return nil
}

func (l *link) initialize(ctx context.Context, protocol EndpointProtocol) error {
	if protocol == ProtocolTUI {
		params := rpc.InitializeParams{ClientInfo: rpc.ClientInfo{Name: "dalgona", Title: "dalgona TUI", Version: config.Version()}}
		if err := l.conn.Initialize(ctx, params, &l.initialized); err != nil {
			return err
		}
		return l.conn.Notify("initialized", nil)
	}
	var err error
	l.native, l.caps, err = handshake(ctx, l.conn)
	if err == nil {
		l.conn.EnableRequestCancellation()
	}
	return err
}

func (l *link) close() error {
	if l == nil {
		return nil
	}
	l.cancel()
	return l.conn.Close()
}

func handshake(ctx context.Context, conn *rpc.Conn) (rpc.NativeSession, rpc.Capabilities, error) {
	params := rpc.InitializeParams{
		ClientInfo:   rpc.ClientInfo{Name: "dalgona", Title: "dalgona desktop", Version: config.Version()},
		Capabilities: &rpc.InitializeCapabilities{DalgonaNative: &rpc.NativeCapability{Version: rpc.NativeVersion, Delivery: rpc.NativeDelivery}},
	}
	var initialized rpc.InitializeResult
	if err := conn.Initialize(ctx, params, &initialized); err != nil {
		return rpc.NativeSession{}, rpc.Capabilities{}, err
	}
	native := initialized.DalgonaNative
	if native == nil || native.Version != rpc.NativeVersion || native.Delivery != rpc.NativeDelivery {
		return rpc.NativeSession{}, rpc.Capabilities{}, errIncompatible
	}
	var caps rpc.Capabilities
	err := conn.Call(ctx, methodCapabilitiesRead, nil, &caps)
	if rpcErr, coded := errors.AsType[*rpc.RPCError](err); coded && rpcErr.Code == methodNotFound {
		err = fmt.Errorf("%w: %s is missing", errIncompatible, methodCapabilitiesRead)
	}
	if err == nil {
		err = checkCapabilities(caps)
	}
	return *native, caps, err
}

func checkCapabilities(caps rpc.Capabilities) error {
	if caps.Version != rpc.NativeVersion {
		return fmt.Errorf("%w: native capabilities version %d is unsupported", errIncompatible, caps.Version)
	}
	if caps.TerminalCheckpointVersion != checkpointVersion {
		return fmt.Errorf("%w: terminal checkpoint version %d is unsupported", errIncompatible, caps.TerminalCheckpointVersion)
	}
	for _, feature := range requiredFeatures {
		if version := caps.Features[feature]; version != 1 {
			return fmt.Errorf("%w: %s version %d does not provide version 1", errIncompatible, feature, version)
		}
	}
	return nil
}

type linkRoute struct {
	link       Link
	observer   Observer
	ctx        context.Context
	announced  chan struct{}
	mu         sync.Mutex
	responders map[string]keptResponder
}

type keptResponder struct {
	respond func(any) error
}

func (r *linkRoute) Notification(method string, params jsontext.Value) {
	if r.announcedLive() {
		r.observer.Notified(r.link, method, params)
	}
}

func (r *linkRoute) Request(ctx context.Context, method string, params jsontext.Value) (any, error) {
	if !r.announcedLive() {
		return nil, ErrStaleLink
	}
	r.keep(ctx)
	return r.observer.Requested(ctx, r.link, method, params)
}

func (r *linkRoute) keep(ctx context.Context) {
	id, err := rpc.ClientRequestID(ctx)
	if err != nil || id == "" {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.responders == nil {
		r.responders = map[string]keptResponder{}
	}
	r.responders[id] = keptResponder{respond: rpc.ClientResponder(ctx)}
}

func (r *linkRoute) respond(requestID string, result any) error {
	r.mu.Lock()
	kept, ok := r.responders[requestID]
	r.mu.Unlock()
	if !ok {
		return fmt.Errorf("client: unknown request %q", requestID)
	}
	return kept.respond(result)
}

func (r *linkRoute) announcedLive() bool {
	select {
	case <-r.announced:
		return r.ctx.Err() == nil
	case <-r.ctx.Done():
		return false
	}
}
