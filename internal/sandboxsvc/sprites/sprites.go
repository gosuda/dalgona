package sprites

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync"

	flysprites "github.com/superfly/sprites-go"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

type Service struct {
	client    *flysprites.Client
	bootstrap runner.BootstrapFunc
}

func New(cli *flysprites.Client, bootstrap runner.BootstrapFunc) *Service {
	return &Service{client: cli, bootstrap: bootstrap}
}

func NewFromToken(token string, bootstrap runner.BootstrapFunc) *Service {
	return New(flysprites.New(token), bootstrap)
}

func (s *Service) Create(ctx context.Context, spec sandboxsvc.Spec) (string, error) {
	if s.client == nil {
		return "", errors.New("sprites: nil client")
	}
	sprite, err := s.client.CreateSprite(ctx, spec.Name, nil)
	if err != nil {
		return "", fmt.Errorf("sprites: create %s: %w", spec.Name, err)
	}
	return sprite.Name(), nil
}

func (s *Service) Attach(ctx context.Context, id string) (string, error) {
	if s.client == nil {
		return "", errors.New("sprites: nil client")
	}
	sprite, err := s.client.GetSprite(ctx, id)
	if err != nil {
		return "", fmt.Errorf("sprites: attach %s: %w", id, err)
	}
	if s.bootstrap != nil {
		run := func(runCtx context.Context, argv []string) (runner.Stream, error) {
			return s.exec(runCtx, sprite, argv)
		}
		if err := s.bootstrap(ctx, id, run); err != nil {
			return "", err
		}
	}
	return id, nil
}

func (s *Service) Destroy(ctx context.Context, id string) error {
	if s.client == nil {
		return errors.New("sprites: nil client")
	}
	sprite, err := s.client.GetSprite(ctx, id)
	if err != nil {
		if isSpriteGone(err) {
			return fmt.Errorf("sprites: attach %s: %w", id, sandboxsvc.ErrGone)
		}
		return fmt.Errorf("sprites: attach %s: %w", id, err)
	}
	if err := sprite.Delete(ctx); err != nil {
		if isSpriteGone(err) {
			return fmt.Errorf("sprites: delete %s: %w", id, sandboxsvc.ErrGone)
		}
		return fmt.Errorf("sprites: delete %s: %w", id, err)
	}
	return nil
}

func isSpriteGone(err error) bool {
	if apiErr, ok := errors.AsType[*flysprites.APIError](err); ok {
		return apiErr.StatusCode == http.StatusNotFound
	}
	return strings.HasPrefix(err.Error(), "sprite not found:")
}

func (s *Service) Connect(ctx context.Context, id string, endpoint sandboxsvc.Endpoint) (io.ReadWriteCloser, error) {
	sprite, err := s.client.GetSprite(ctx, id)
	if err != nil {
		return nil, fmt.Errorf("sprites: attach %s: %w", id, err)
	}
	argv := []string{"sh", "-c", `exec "$HOME/.dalgona/bin/dalgona" exec-server --listen stdio:// --root /workspace`}
	if endpoint == sandboxsvc.Bridge {
		argv = []string{"sh", "-c", `exec "$HOME/.dalgona/bin/dalgona" bridge`}
	}
	return s.exec(ctx, sprite, argv)
}

func (s *Service) exec(ctx context.Context, sprite *flysprites.Sprite, argv []string) (runner.Stream, error) {
	if len(argv) == 0 {
		return nil, errors.New("sprites: empty command")
	}
	cmd := sprite.CommandContext(ctx, argv[0], argv[1:]...)
	cmd.Dir = "/workspace"
	stdin, stdinWriter := io.Pipe()
	stdout, stdoutWriter := io.Pipe()
	cmd.Stdin = stdin
	cmd.Stdout = stdoutWriter
	cmd.Stderr = stdoutWriter
	if err := cmd.Start(); err != nil {
		_ = stdin.Close()
		_ = stdinWriter.Close()
		_ = stdout.Close()
		_ = stdoutWriter.Close()
		return nil, fmt.Errorf("sprites: start command: %w", err)
	}
	stream := &stream{
		input:        stdinWriter,
		output:       stdout,
		outputWriter: stdoutWriter,
		done:         make(chan struct{}),
	}
	go stream.await(cmd.Wait, cmd.ExitCode)
	return stream, nil
}

type stream struct {
	input        *io.PipeWriter
	output       *io.PipeReader
	outputWriter *io.PipeWriter
	closeOnce    sync.Once
	done         chan struct{}
	waitCode     int
	waitErr      error
}

func (s *stream) Read(p []byte) (int, error)  { return s.output.Read(p) }
func (s *stream) Write(p []byte) (int, error) { return s.input.Write(p) }
func (s *stream) CloseWrite() error           { return s.input.Close() }
func (s *stream) Close() error {
	var err error
	s.closeOnce.Do(func() {
		err = errors.Join(s.input.Close(), s.output.Close())
	})
	return err
}

func (s *stream) Wait() (int, error) {
	<-s.done
	return s.waitCode, s.waitErr
}

func (s *stream) await(wait func() error, exitCode func() int) {
	err := wait()
	code := exitCode()
	if _, ok := errors.AsType[*flysprites.ExitError](err); ok {
		err = nil
	} else if err != nil {
		err = fmt.Errorf("sprites: wait for command: %w", err)
	}
	s.waitCode = code
	s.waitErr = err
	_ = s.outputWriter.Close()
	close(s.done)
}
