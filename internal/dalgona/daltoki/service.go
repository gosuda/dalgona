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
	Enabled           bool
	Running           bool
	PublicationPaused bool
	LastError         string
}

type Service struct {
	cfg        Config
	deps       Dependencies
	store      *serviceStore
	mu         sync.Mutex
	host       *agent.Host
	running    bool
	closed     bool
	publishing bool
	reclaimer  threadReclaimer
	active     int
	wakeup     chan struct{}
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
		return &Service{cfg: cfg, deps: deps, wakeup: make(chan struct{}, 1)}, nil
	}
	if err := requireDependencies(deps); err != nil {
		return nil, err
	}
	store, err := openServiceStore(ctx, serviceJournalDir(deps.Home), deps.Clock)
	if err != nil {
		return nil, err
	}
	return &Service{cfg: cfg, deps: deps, store: store, wakeup: make(chan struct{}, 1)}, nil
}

func requireDependencies(deps Dependencies) error {
	switch {
	case deps.Home == "":
		return errors.New("daltoki: home is required")
	case deps.Client == nil:
		return errors.New("daltoki: github client is required")
	case deps.Auth == nil:
		return errors.New("daltoki: auth store is required")
	case deps.Graphs == nil:
		return errors.New("daltoki: graph scheduler is required")
	case deps.Worktrees == nil:
		return errors.New("daltoki: worktree manager is required")
	}
	return nil
}

func serviceJournalDir(home string) string {
	return home + string(filepath.Separator) + "daltoki"
}

func (s *Service) Attach(host *agent.Host) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.host = host
	if s.deps.Graphs != nil {
		s.deps.Graphs.Attach(host)
	}
}

func (s *Service) StartAfterRecovery(recoverErr error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if recoverErr != nil {
		s.startupErr = errors.New("daltoki: host recovery failed")
	}
}

func (s *Service) begin(ctx context.Context) (context.Context, bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if !s.cfg.Enabled || s.closed || s.running {
		return nil, false, nil
	}
	if s.startupErr == nil {
		s.startupErr = s.store.Damaged()
	}
	if s.startupErr != nil {
		return nil, false, s.startupErr
	}
	s.running = true
	s.publishing = !s.store.ShutdownHeld()
	runCtx, cancel := context.WithCancel(context.WithoutCancel(ctx))
	s.runCancel = cancel
	return runCtx, true, nil
}

func (s *Service) Run(ctx context.Context) error {
	runCtx, started, err := s.begin(ctx)
	if !started {
		return err
	}
	if err := s.start(runCtx); err != nil {
		s.mu.Lock()
		s.runCancel()
		s.running = false
		s.startupErr = err
		s.mu.Unlock()
		return err
	}
	select {
	case <-ctx.Done():
		if closeErr := s.Close(context.WithoutCancel(ctx)); closeErr != nil {
			return errors.Join(ctx.Err(), closeErr)
		}
		return ctx.Err()
	case <-runCtx.Done():
	}
	s.mu.Lock()
	s.running = false
	closed := s.closed
	s.mu.Unlock()
	if closed {
		return nil
	}
	return ctx.Err()
}

func (s *Service) start(ctx context.Context) error {
	if err := s.recoverState(ctx); err != nil {
		return err
	}
	if err := s.startIntake(ctx); err != nil {
		return err
	}
	s.workers.Add(1)
	go func() {
		defer s.workers.Done()
		s.dispatchLoop(ctx)
	}()
	return nil
}

func (s *Service) Close(ctx context.Context) error {
	s.mu.Lock()
	if s.closeDone {
		err := s.closeErr
		s.mu.Unlock()
		return err
	}
	s.closed = true
	s.publishing = false
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
	if s.store != nil {
		if workersErr == nil {
			errs = append(errs, s.store.Close())
		} else {
			workers := &s.workers
			store := s.store
			go func() {
				workers.Wait()
				_ = store.Close()
			}()
		}
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
	running := s.running && !s.closed
	status := Status{Enabled: s.cfg.Enabled, Running: running, PublicationPaused: running && !s.publishing}
	if s.startupErr != nil {
		status.LastError = s.startupErr.Error()
	} else if s.closeErr != nil {
		status.LastError = s.closeErr.Error()
	}
	return status
}

func (s *Service) startIntake(ctx context.Context) error {
	poller := NewPoller(s.deps.Client, pollAdmission{s: s}, PollerConfig{
		BaseURL:      s.deps.Client.APIBase(),
		Repositories: s.cfg.Repositories,
		Interval:     s.cfg.PollInterval,
	}, s.deps.Clock, s.deps.Logger)
	var intake *RelayIntake
	if s.cfg.RelayURL != "" {
		relay, err := NewRelayClient(s.cfg.RelayURL, s.cfg.RelayToken, nil)
		if err != nil {
			return err
		}
		intake = NewRelayIntake(relay, s.cfg.WebhookSecret, envelopeStore{s: s}, s.deps.Clock)
		intake.SaveAfter = s.store.SaveRelayAfter
	}
	s.workers.Add(1)
	go func() {
		defer s.workers.Done()
		if err := poller.Run(ctx, 0); err != nil && ctx.Err() == nil {
			s.deps.Logger.Error("daltoki: poller stopped", "error", err)
		}
	}()
	if intake == nil {
		return nil
	}
	after := s.store.LoadRelayAfter()
	s.workers.Add(1)
	go func() {
		defer s.workers.Done()
		if err := intake.Run(ctx, after); err != nil && ctx.Err() == nil {
			s.deps.Logger.Error("daltoki: relay intake stopped", "error", err)
		}
	}()
	return nil
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
