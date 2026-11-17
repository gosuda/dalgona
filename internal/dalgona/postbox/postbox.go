package postbox

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"sync/atomic"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

const (
	StateQueued   = "queued"
	StateRunning  = "running"
	StateIdle     = "idle"
	StateFinished = "finished"
	StateFailed   = "failed"
	StateCrashed  = "crashed"
	StateArchived = "archived"
)

type Options struct {
	Host            *agent.Host
	Sessions        bool
	UserID          string
	ContinueForMail func(ctx context.Context, threadID string) error
}

type Endpoint struct {
	ThreadID  string
	SessionID string
	AgentID   string
	State     string
}

type Request struct {
	OperationID string
	To          string
	Text        string
	ReplyTo     string
}

type ReceiptKind string

const (
	ReceiptBuffered  ReceiptKind = "buffered"
	ReceiptDelivered ReceiptKind = "delivered"
	ReceiptFull      ReceiptKind = "full"
	ReceiptGone      ReceiptKind = "gone"
)

type Receipt struct {
	MessageID string
	PairSeq   uint64
	Kind      ReceiptKind
	Delivery  string
}

type Service struct {
	host            *agent.Host
	sessions        bool
	userID          string
	continueForMail func(ctx context.Context, threadID string) error

	mu       sync.Mutex
	byThread map[string]Endpoint
	byAddr   map[mailKey]string

	queue  chan routedPost
	cancel context.CancelFunc
	wg     sync.WaitGroup
	closed atomic.Bool
}

type mailKey struct {
	session string
	agent   string
}

type routedPost struct {
	ctx        context.Context
	sender     *agent.Thread
	senderID   string
	senderTurn string
	from       event.MailAddress
	request    Request
	reply      chan postResult
}

type postResult struct {
	receipt Receipt
	err     error
}

func New(ctx context.Context, options Options) (*Service, error) {
	if options.Host == nil {
		return nil, errors.New("postbox: host is required")
	}
	owned, cancel := context.WithCancel(context.WithoutCancel(ctx))
	s := &Service{
		host:            options.Host,
		sessions:        options.Sessions,
		userID:          options.UserID,
		continueForMail: options.ContinueForMail,
		byThread:        make(map[string]Endpoint),
		byAddr:          make(map[mailKey]string),
		queue:           make(chan routedPost, 32),
		cancel:          cancel,
	}
	s.wg.Add(1)
	go s.route(owned)
	return s, nil
}

func (s *Service) route(ctx context.Context) {
	defer s.wg.Done()
	for {
		select {
		case <-ctx.Done():
			s.drainQueue()
			return
		case pending := <-s.queue:
			pending.ctx = ctx
			pending.reply <- s.handle(pending)
		}
	}
}

func (s *Service) drainQueue() {
	for {
		select {
		case pending := <-s.queue:
			pending.reply <- postResult{err: errors.New("post: service closed")}
		default:
			return
		}
	}
}

func (s *Service) Close(ctx context.Context) error {
	if s.closed.Swap(true) {
		return nil
	}
	s.cancel()
	done := make(chan struct{})
	go func() { s.wg.Wait(); close(done) }()
	select {
	case <-done:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (s *Service) Register(ctx context.Context, endpoint Endpoint) error {
	derived, err := s.resolveRegisterEndpoint(ctx, endpoint)
	if err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.storeRegisterEndpoint(derived)
}

func (s *Service) resolveRegisterEndpoint(ctx context.Context, endpoint Endpoint) (Endpoint, error) {
	if endpoint.ThreadID == "" {
		return Endpoint{}, fmt.Errorf("post: unknown endpoint %q", endpoint.ThreadID)
	}
	_, state, err := s.threadStateContext(ctx, endpoint.ThreadID)
	if err != nil {
		return Endpoint{}, err
	}
	derived := deriveIdentity(s.host, endpoint.ThreadID, endpoint.AgentID, state)
	if derived.AgentID == "" {
		return Endpoint{}, errors.New("post: endpoint identity conflict")
	}
	endpointState, err := s.deriveState(endpoint.ThreadID)
	if err != nil {
		return Endpoint{}, err
	}
	derived.State = endpointState
	return derived, nil
}

func (s *Service) storeRegisterEndpoint(derived Endpoint) error {
	if existing, ok := s.byThread[derived.ThreadID]; ok {
		if existing.SessionID != derived.SessionID || existing.AgentID != derived.AgentID {
			return errors.New("post: endpoint identity conflict")
		}
		existing.State = derived.State
		s.byThread[derived.ThreadID] = existing
		return nil
	}
	if owner, ok := s.byAddr[mailKey{session: derived.SessionID, agent: derived.AgentID}]; ok && owner != derived.ThreadID {
		return errors.New("post: endpoint identity conflict")
	}
	s.byThread[derived.ThreadID] = derived
	s.byAddr[mailKey{session: derived.SessionID, agent: derived.AgentID}] = derived.ThreadID
	return nil
}

func deriveIdentity(host *agent.Host, threadID, agentID string, state event.State) Endpoint {
	root := host.SessionRoot(threadID)
	derived := "main"
	if state.Parent != "" || state.Advisor != "" {
		if agentID == "" {
			agentID = threadID
		}
		derived = agentID
	} else if agentID != "" && agentID != "main" {
		return Endpoint{}
	}
	return Endpoint{ThreadID: threadID, SessionID: root, AgentID: derived}
}

func (s *Service) deriveState(threadID string) (string, error) {
	if s.isArchivedThread(threadID) {
		return StateArchived, nil
	}
	thread, err := s.host.Resume(context.Background(), threadID)
	if err != nil {
		return "", fmt.Errorf("post: unknown endpoint %q", threadID)
	}
	status, err := thread.Status()
	if err != nil {
		return "", fmt.Errorf("post: unknown endpoint %q", threadID)
	}
	return stateForStatus(status.State), nil
}

func (s *Service) isArchivedThread(threadID string) bool {
	for _, info := range s.host.AllThreads() {
		if info.ID != threadID {
			continue
		}
		return info.Archived
	}
	return false
}

func stateForStatus(state event.Status) string {
	switch state {
	case event.StatusIdle:
		return StateIdle
	case event.StatusTurnRunning, event.StatusAwaitingApproval:
		return StateRunning
	case event.StatusDamaged:
		return StateCrashed
	default:
		return StateIdle
	}
}

func (s *Service) Transition(ctx context.Context, threadID, state string) error {
	switch state {
	case StateQueued, StateRunning, StateIdle, StateFinished, StateFailed, StateCrashed, StateArchived:
	default:
		return fmt.Errorf("post: invalid endpoint state %q", state)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.byThread[threadID]; ok {
		endpoint := s.byThread[threadID]
		endpoint.State = state
		s.byThread[threadID] = endpoint
		return nil
	}
	_, record, err := s.threadStateContext(ctx, threadID)
	if err != nil {
		return err
	}
	derived := deriveIdentity(s.host, threadID, "", record)
	if derived.AgentID == "" {
		return errors.New("post: endpoint identity conflict")
	}
	derived.State = state
	s.byThread[threadID] = derived
	s.byAddr[mailKey{session: derived.SessionID, agent: derived.AgentID}] = threadID
	return nil
}

type Sender struct {
	service  *Service
	thread   *agent.Thread
	threadID string
	turnID   string
	from     event.MailAddress
}

func (s *Service) Bind(ctx context.Context, threadID, turnID string) (*Sender, error) {
	thread, state, err := s.threadStateContext(ctx, threadID)
	if err != nil {
		return nil, err
	}
	if state.Advisor != "" {
		return nil, fmt.Errorf("post: thread %q cannot send mail", threadID)
	}
	if turnID == "" {
		return nil, errors.New("post: invalid originating operation")
	}
	if state.TurnID != turnID {
		if _, ok := state.CompletedTurns[turnID]; !ok {
			return nil, errors.New("post: invalid originating operation")
		}
	}
	root := s.host.SessionRoot(threadID)
	from := event.MailAddress{SessionID: root, AgentID: "main"}
	if state.Parent != "" {
		from.AgentID = threadID
	}
	return &Sender{service: s, thread: thread, threadID: threadID, turnID: turnID, from: from}, nil
}

func (s *Service) threadStateContext(ctx context.Context, threadID string) (*agent.Thread, event.State, error) {
	thread, err := s.host.Resume(ctx, threadID)
	if err != nil {
		return nil, event.State{}, fmt.Errorf("post: unknown endpoint %q", threadID)
	}
	_, state, err := thread.Records()
	if err != nil {
		return nil, event.State{}, fmt.Errorf("post: unknown endpoint %q", threadID)
	}
	return thread, state, nil
}

func (snd *Sender) Post(ctx context.Context, request Request) (Receipt, error) {
	if snd.service.closed.Load() {
		return Receipt{}, errors.New("post: service closed")
	}
	reply := make(chan postResult, 1)
	pending := routedPost{
		ctx:        ctx,
		sender:     snd.thread,
		senderID:   snd.threadID,
		senderTurn: snd.turnID,
		from:       snd.from,
		request:    request,
		reply:      reply,
	}
	select {
	case snd.service.queue <- pending:
	case <-ctx.Done():
		return Receipt{}, ctx.Err()
	}
	select {
	case result := <-reply:
		return result.receipt, result.err
	case <-ctx.Done():
		return Receipt{}, ctx.Err()
	}
}

type EndpointLifecycle interface {
	Register(ctx context.Context, endpoint agent.MailEndpoint) error
	Transition(ctx context.Context, threadID, state string) error
}

type LifecycleAdapter struct {
	service *Service
}

func NewLifecycleAdapter(service *Service) *LifecycleAdapter {
	return &LifecycleAdapter{service: service}
}

func (a *LifecycleAdapter) Register(ctx context.Context, endpoint agent.MailEndpoint) error {
	return a.service.Register(ctx, Endpoint{
		ThreadID:  endpoint.ThreadID,
		SessionID: endpoint.SessionID,
		AgentID:   endpoint.AgentID,
		State:     endpoint.State,
	})
}

func (a *LifecycleAdapter) Transition(ctx context.Context, threadID, state string) error {
	return a.service.Transition(ctx, threadID, state)
}
