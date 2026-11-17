package daltoki

import (
	"context"
	"errors"
	"log/slog"
	"path/filepath"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dalgona/github"
)

const journalSession = "daltoki"

type GraphScheduler interface {
	CancelAll(ctx context.Context) error
}

type WorktreeManager interface {
	Close() error
}

type Dependencies struct {
	Home      string
	Client    *github.Client
	Graphs    GraphScheduler
	Worktrees WorktreeManager
	Auth      *auth.Store
	Clock     func() time.Time
	Logger    *slog.Logger
}

type Status struct {
	Enabled   bool
	Running   bool
	LastError string
}

type Service struct {
	cfg        Config
	deps       Dependencies
	store      *serviceStore
	mu         sync.Mutex
	host       *agent.Host
	running    bool
	closed     bool
	startupErr error
	runCancel  context.CancelFunc
	workers    sync.WaitGroup
	closeErr   error
	closeDone  bool
}

func New(ctx context.Context, cfg Config, deps Dependencies) (*Service, error) {
	if ctx.Err() != nil {
		return nil, ctx.Err()
	}
	if deps.Logger == nil {
		deps.Logger = slog.Default()
	}
	if deps.Clock == nil {
		deps.Clock = time.Now
	}
	if !cfg.Enabled {
		return &Service{cfg: cfg, deps: deps}, nil
	}
	if deps.Home == "" {
		return nil, errors.New("daltoki: home is required")
	}
	if deps.Client == nil {
		return nil, errors.New("daltoki: github client is required")
	}
	if deps.Auth == nil {
		return nil, errors.New("daltoki: auth store is required")
	}
	store, err := openServiceStore(ctx, serviceJournalDir(deps.Home), deps.Clock)
	if err != nil {
		return nil, err
	}
	return &Service{cfg: cfg, deps: deps, store: store}, nil
}

func serviceJournalDir(home string) string {
	return home + string(filepath.Separator) + "daltoki"
}

func (s *Service) Attach(host *agent.Host) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.host = host
}

func (s *Service) StartAfterRecovery(recoverErr error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if recoverErr != nil {
		s.startupErr = errors.New("daltoki: host recovery failed")
	}
}

func (s *Service) Run(ctx context.Context) error {
	s.mu.Lock()
	if !s.cfg.Enabled || s.closed || s.running {
		s.mu.Unlock()
		return nil
	}
	if s.startupErr != nil {
		err := s.startupErr
		s.mu.Unlock()
		return err
	}
	s.running = true
	runCtx, cancel := context.WithCancel(ctx)
	s.runCancel = cancel
	s.mu.Unlock()
	if err := s.startIntake(runCtx); err != nil {
		cancel()
		s.mu.Lock()
		s.running = false
		s.mu.Unlock()
		return err
	}
	<-runCtx.Done()
	s.mu.Lock()
	s.running = false
	closed := s.closed
	s.mu.Unlock()
	if closed {
		return nil
	}
	return ctx.Err()
}

func (s *Service) Close(ctx context.Context) error {
	s.mu.Lock()
	if s.closeDone {
		err := s.closeErr
		s.mu.Unlock()
		return err
	}
	s.closed = true
	cancel := s.runCancel
	s.mu.Unlock()
	bounded, stop := context.WithTimeout(ctx, 10*time.Second)
	defer stop()
	barrierErr := s.recordBarrier(bounded)
	if cancel != nil {
		cancel()
	}
	var errs []error
	errs = append(errs, barrierErr)
	if s.deps.Graphs != nil {
		errs = append(errs, s.deps.Graphs.CancelAll(bounded))
	}
	if s.deps.Worktrees != nil {
		errs = append(errs, s.deps.Worktrees.Close())
	}
	workersErr := waitWorkers(bounded, &s.workers)
	errs = append(errs, workersErr)
	if workersErr == nil && s.store != nil {
		errs = append(errs, s.store.Close())
	}
	err := errors.Join(errs...)
	s.mu.Lock()
	s.closeErr = err
	s.closeDone = true
	s.mu.Unlock()
	return err
}

func (s *Service) Status() Status {
	s.mu.Lock()
	defer s.mu.Unlock()
	status := Status{Enabled: s.cfg.Enabled, Running: s.running && !s.closed}
	if s.startupErr != nil {
		status.LastError = s.startupErr.Error()
	} else if s.closeErr != nil {
		status.LastError = s.closeErr.Error()
	}
	return status
}

func (s *Service) startIntake(ctx context.Context) error {
	poller := NewPoller(s.deps.Client, s.store, PollerConfig{
		BaseURL:      s.deps.Client.APIBase(),
		Repositories: s.cfg.Repositories,
		Interval:     s.cfg.PollInterval,
	}, s.deps.Clock, s.deps.Logger)
	s.workers.Add(1)
	go func() {
		defer s.workers.Done()
		if err := poller.Run(ctx, 0); err != nil && ctx.Err() == nil {
			s.deps.Logger.Error("daltoki: poller stopped", "error", err)
		}
	}()
	if s.cfg.RelayURL != "" {
		relay, err := NewRelayClient(s.cfg.RelayURL, s.cfg.RelayToken, nil)
		if err != nil {
			return err
		}
		intake := NewRelayIntake(relay, s.cfg.WebhookSecret, envelopeStore{store: s.store}, s.deps.Clock)
		intake.SaveAfter = s.store.SaveRelayAfter
		after := s.store.LoadRelayAfter()
		s.workers.Add(1)
		go func() {
			defer s.workers.Done()
			if err := intake.Run(ctx, after); err != nil && ctx.Err() == nil {
				s.deps.Logger.Error("daltoki: relay intake stopped", "error", err)
			}
		}()
	}
	return nil
}

type envelopeStore struct {
	store *serviceStore
}

func (e envelopeStore) Admit(ctx context.Context, envelope Envelope) (bool, error) {
	return e.store.AdmitEnvelope(ctx, envelope)
}

func waitWorkers(ctx context.Context, workers *sync.WaitGroup) error {
	done := make(chan struct{})
	go func() {
		workers.Wait()
		close(done)
	}()
	select {
	case <-done:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (s *Service) recordBarrier(ctx context.Context) error {
	if s.store == nil {
		return nil
	}
	return s.store.RecordControl(ctx, controlRecord{Shutdown: true})
}
