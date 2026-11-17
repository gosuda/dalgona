package daltoki

import (
	"context"
	json "encoding/json/v2"
	"errors"
	"log/slog"
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

type Journal interface {
	Append(ctx context.Context, session string, events ...[]byte) (uint64, error)
}

type Dependencies struct {
	Journal   Journal
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
	if deps.Journal == nil {
		return nil, errors.New("daltoki: journal is required")
	}
	if deps.Client == nil {
		return nil, errors.New("daltoki: github client is required")
	}
	if deps.Auth == nil {
		return nil, errors.New("daltoki: auth store is required")
	}
	return &Service{cfg: cfg, deps: deps}, nil
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
	errs = append(errs, waitWorkers(bounded, &s.workers))
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

type barrierRecord struct {
	Version int            `json:"version"`
	Kind    string         `json:"kind"`
	At      time.Time      `json:"at"`
	Control barrierControl `json:"control"`
}

type barrierControl struct {
	Shutdown bool `json:"shutdown"`
}

func (s *Service) recordBarrier(ctx context.Context) error {
	if s.deps.Journal == nil {
		return nil
	}
	record := barrierRecord{
		Version: 1,
		Kind:    "daltoki.control.changed",
		At:      s.deps.Clock().UTC(),
		Control: barrierControl{Shutdown: true},
	}
	raw, err := json.Marshal(record)
	if err != nil {
		return err
	}
	_, err = s.deps.Journal.Append(ctx, journalSession, raw)
	return err
}
