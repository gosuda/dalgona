package sandbox0

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
	sdk "github.com/sandbox0-ai/sdk-go"
)

type Service struct {
	client    *sdk.Client
	bootstrap runner.BootstrapFunc
	log       *slog.Logger
}

func New(client *sdk.Client, bootstrap runner.BootstrapFunc, logger *slog.Logger) *Service {
	if logger == nil {
		logger = slog.Default()
	}
	return &Service{client: client, bootstrap: bootstrap, log: logger}
}

func NewFromEnv(_ context.Context, bootstrap runner.BootstrapFunc, logger *slog.Logger) (*Service, error) {
	token := os.Getenv("SANDBOX0_TOKEN")
	if token == "" {
		return nil, errors.New("sandbox0: SANDBOX0_TOKEN is not set")
	}
	opts := []sdk.Option{sdk.WithToken(token)}
	if base := os.Getenv("SANDBOX0_BASE_URL"); base != "" {
		opts = append(opts, sdk.WithBaseURL(base))
	}
	client, err := sdk.NewClient(opts...)
	if err != nil {
		return nil, fmt.Errorf("sandbox0: create client: %w", err)
	}
	return New(client, bootstrap, logger), nil
}

func (s *Service) Create(ctx context.Context, spec sandboxsvc.Spec) (string, error) {
	template := cmp.Or(spec.Template, "default")
	box, err := s.client.ClaimSandbox(ctx, template, sdk.WithSandboxTTL(1800), sdk.WithSandboxHardTTL(0), sdk.WithSandboxAutoResume(false))
	if err != nil {
		return "", fmt.Errorf("sandbox0: claim sandbox: %w", err)
	}
	if box == nil {
		return "", errors.New("sandbox0: claim sandbox returned no box")
	}
	if err := s.ensureWorkspace(ctx, box.ID, template); err != nil {
		cleanupCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), time.Minute)
		defer cancel()
		_, cleanupErr := s.client.DeleteSandbox(cleanupCtx, box.ID)
		if cleanupErr != nil {
			return "", errors.Join(err, fmt.Errorf("sandbox0: cleanup %s: %w", box.ID, cleanupErr))
		}
		return "", err
	}
	return box.ID, nil
}

type workspaceError struct {
	template string
	cause    error
}

func (e *workspaceError) Error() string {
	return fmt.Sprintf("runner: sandbox0 template %s cannot create /workspace; use a template whose user can write it", e.template)
}

func (e *workspaceError) Unwrap() error {
	return e.cause
}

func (s *Service) ensureWorkspace(ctx context.Context, id, template string) error {
	box := s.client.Sandbox(id)
	argv := []string{"sh", "-c", `mkdir -p /workspace 2>/dev/null || { sudo -n mkdir -p /workspace && sudo -n chown "$(id -u)" /workspace; }`}
	result, err := box.Cmd(ctx, argv[0], sdk.WithCommand(argv))
	if err == nil && result.ExitCode != nil && *result.ExitCode == 0 {
		return nil
	}
	if err == nil {
		err = errors.New("workspace command failed")
	}
	return &workspaceError{template: template, cause: err}
}

func (s *Service) Attach(ctx context.Context, id string) (string, error) {
	box, err := s.client.GetSandbox(ctx, id)
	if err != nil {
		return "", fmt.Errorf("sandbox0: attach %s: %w", id, err)
	}
	if box == nil {
		return "", fmt.Errorf("sandbox0: attach %s returned no box", id)
	}
	if box.Paused {
		if _, err := s.client.ResumeSandboxAndWait(ctx, id, nil); err != nil {
			return "", fmt.Errorf("sandbox0: resume %s: %w", id, err)
		}
	}
	if s.bootstrap != nil {
		run := func(runCtx context.Context, argv []string) (runner.Stream, error) {
			return s.start(runCtx, id, argv)
		}
		if err := s.bootstrap(ctx, id, run); err != nil {
			return "", fmt.Errorf("sandbox0: bootstrap %s: %w", id, err)
		}
	}
	return id, nil
}

func (s *Service) Destroy(ctx context.Context, id string) error {
	if _, err := s.client.DeleteSandbox(ctx, id); err != nil {
		return fmt.Errorf("sandbox0: delete %s: %w", id, err)
	}
	return nil
}

func (s *Service) Connect(ctx context.Context, id string, endpoint sandboxsvc.Endpoint) (io.ReadWriteCloser, error) {
	argv := []string{"sh", "-c", `exec "$HOME/.dalgona/bin/dalgona" exec-server --listen stdio:// --root /workspace`}
	if endpoint == sandboxsvc.Bridge {
		argv = []string{"sh", "-c", `exec "$HOME/.dalgona/bin/dalgona" bridge`}
	}
	return s.start(ctx, id, argv)
}

func (s *Service) start(ctx context.Context, id string, argv []string) (runner.Stream, error) {
	if len(argv) == 0 {
		return nil, errors.New("sandbox0: empty command")
	}
	stream, err := s.client.Sandbox(id).CmdStream(ctx, argv[0], sdk.WithCommand(argv), sdk.WithCmdCWD("/workspace"))
	if err != nil {
		return nil, fmt.Errorf("sandbox0: start process: %w", err)
	}
	return s.refreshed(ctx, id, &processStream{Stream: NewStream(stream, s.log), remote: stream}), nil
}

func (s *Service) refreshed(ctx context.Context, id string, stream *processStream) runner.Stream {
	r := &refreshingStream{processStream: stream, stop: make(chan struct{})}
	go func() {
		ticker := time.NewTicker(10 * time.Minute)
		defer ticker.Stop()
		for {
			select {
			case <-ticker.C:
				if _, err := s.client.RefreshSandbox(ctx, id, nil); err != nil {
					s.log.Debug("sandbox0 refresh failed", "id", id, "error", err)
				}
			case <-r.stop:
				return
			case <-ctx.Done():
				return
			}
		}
	}()
	return r
}

type processStream struct {
	*Stream
	remote *sdk.ContextStream
}

func (p *processStream) Wait() (int, error) {
	done, err := p.remote.Wait()
	if err != nil {
		return 0, fmt.Errorf("sandbox0: wait for process: %w", err)
	}
	if done.ExitCode == nil {
		return 0, nil
	}
	return *done.ExitCode, nil
}

type refreshingStream struct {
	*processStream
	stop      chan struct{}
	closeOnce sync.Once
}

func (r *refreshingStream) Close() error {
	r.closeOnce.Do(func() {
		close(r.stop)
	})
	return r.processStream.Close()
}

var _ sandboxsvc.Service = (*Service)(nil)
