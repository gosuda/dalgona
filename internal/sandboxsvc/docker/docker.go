package docker

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"io"
	"sync"
	"time"

	cerrdefs "github.com/containerd/errdefs"
	"github.com/moby/moby/api/pkg/stdcopy"
	"github.com/moby/moby/api/types/container"
	"github.com/moby/moby/client"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

type Service struct {
	client    *client.Client
	bootstrap runner.BootstrapFunc
}

var _ sandboxsvc.Service = (*Service)(nil)

func New(cli *client.Client, bootstrap runner.BootstrapFunc) *Service {
	return &Service{client: cli, bootstrap: bootstrap}
}

func NewFromEnv(bootstrap runner.BootstrapFunc) (*Service, error) {
	cli, err := client.New(client.FromEnv)
	if err != nil {
		return nil, fmt.Errorf("docker: create client: %w", err)
	}
	return New(cli, bootstrap), nil
}

func (s *Service) Create(ctx context.Context, spec sandboxsvc.Spec) (string, error) {
	if s.client == nil {
		return "", errors.New("docker: nil client")
	}
	image := cmp.Or(spec.Image, "docker.io/library/buildpack-deps:stable")
	result, err := s.createContainer(ctx, spec.Name, image)
	if cerrdefs.IsNotFound(err) {
		if pullErr := s.pullImage(ctx, image); pullErr != nil {
			return "", pullErr
		}
		result, err = s.createContainer(ctx, spec.Name, image)
	}
	if err != nil {
		return "", fmt.Errorf("docker: create container: %w", err)
	}
	if _, startErr := s.client.ContainerStart(ctx, result.ID, client.ContainerStartOptions{}); startErr != nil {
		startFailure := fmt.Errorf("docker: start container %s: %w", result.ID, startErr)
		_, cleanupErr := s.client.ContainerRemove(context.Background(), result.ID, client.ContainerRemoveOptions{Force: true})
		if cleanupErr != nil {
			cleanupFailure := fmt.Errorf("docker: remove container %s after start failure: %w", result.ID, cleanupErr)
			return "", errors.Join(startFailure, cleanupFailure)
		}
		return "", startFailure
	}
	return result.ID, nil
}

func (s *Service) createContainer(ctx context.Context, name, image string) (client.ContainerCreateResult, error) {
	return s.client.ContainerCreate(ctx, client.ContainerCreateOptions{
		Name:  name,
		Image: image,
		Config: &container.Config{
			Cmd:        []string{"sleep", "infinity"},
			WorkingDir: "/workspace",
		},
		HostConfig: &container.HostConfig{RestartPolicy: container.RestartPolicy{Name: container.RestartPolicyUnlessStopped}},
	})
}

func (s *Service) pullImage(ctx context.Context, image string) error {
	response, err := s.client.ImagePull(ctx, image, client.ImagePullOptions{})
	if err != nil {
		return fmt.Errorf("docker: pull image %s: %w", image, err)
	}
	if err := response.Wait(ctx); err != nil {
		return fmt.Errorf("docker: pull image %s: %w", image, err)
	}
	return nil
}

func (s *Service) Attach(ctx context.Context, id string) (string, error) {
	if s.client == nil {
		return "", errors.New("docker: nil client")
	}
	inspected, err := s.client.ContainerInspect(ctx, id, client.ContainerInspectOptions{})
	if err != nil {
		return "", fmt.Errorf("docker: inspect container %s: %w", id, err)
	}
	if inspected.Container.State == nil || !inspected.Container.State.Running {
		if _, startErr := s.client.ContainerStart(ctx, id, client.ContainerStartOptions{}); startErr != nil {
			return "", fmt.Errorf("docker: start container %s: %w", id, startErr)
		}
	}
	if s.bootstrap != nil {
		run := func(runCtx context.Context, argv []string) (runner.Stream, error) {
			return s.exec(runCtx, id, argv)
		}
		if err := s.bootstrap(ctx, id, run); err != nil {
			return "", err
		}
	}
	return id, nil
}

func (s *Service) Destroy(ctx context.Context, id string) error {
	if s.client == nil {
		return errors.New("docker: nil client")
	}
	if _, err := s.client.ContainerRemove(ctx, id, client.ContainerRemoveOptions{Force: true}); err != nil {
		if cerrdefs.IsNotFound(err) {
			return fmt.Errorf("docker: remove container %s: %w", id, sandboxsvc.ErrGone)
		}
		return fmt.Errorf("docker: remove container %s: %w", id, err)
	}
	return nil
}

func (s *Service) Connect(ctx context.Context, id string, endpoint sandboxsvc.Endpoint) (io.ReadWriteCloser, error) {
	argv := []string{"sh", "-c", `exec "$HOME/.dalgona/bin/dalgona" exec-server --listen stdio:// --root /workspace`}
	if endpoint == sandboxsvc.Bridge {
		argv = []string{"sh", "-c", `exec "$HOME/.dalgona/bin/dalgona" bridge`}
	}
	return s.exec(ctx, id, argv)
}

func (s *Service) exec(ctx context.Context, id string, argv []string) (runner.Stream, error) {
	if len(argv) == 0 {
		return nil, errors.New("docker: empty command")
	}
	result, err := s.client.ExecCreate(ctx, id, client.ExecCreateOptions{
		Cmd:          argv,
		AttachStdin:  true,
		AttachStdout: true,
		AttachStderr: true,
		TTY:          false,
		WorkingDir:   "/workspace",
	})
	if err != nil {
		return nil, fmt.Errorf("docker: create exec: %w", err)
	}
	attached, err := s.client.ExecAttach(ctx, result.ID, client.ExecAttachOptions{TTY: false})
	if err != nil {
		return nil, fmt.Errorf("docker: attach exec: %w", err)
	}
	stream := newStream(s.client, result.ID, attached)
	return stream, nil
}

type stream struct {
	resp     client.HijackedResponse
	in       io.Writer
	out      *io.PipeReader
	outClose io.Closer
	done     chan struct{}
	once     sync.Once
	client   *client.Client
	execID   string
	copyErr  error
	mu       sync.Mutex
}

func newStream(cli *client.Client, execID string, resp client.ExecAttachResult) *stream {
	reader, writer := io.Pipe()
	st := &stream{resp: resp.HijackedResponse, in: resp.Conn, out: reader, outClose: writer, done: make(chan struct{}), client: cli, execID: execID}
	go func() {
		_, err := stdcopy.StdCopy(writer, writer, resp.Reader)
		st.mu.Lock()
		st.copyErr = err
		st.mu.Unlock()
		_ = writer.CloseWithError(err)
		close(st.done)
	}()
	return st
}

func (s *stream) Read(p []byte) (int, error)  { return s.out.Read(p) }
func (s *stream) Write(p []byte) (int, error) { return s.in.Write(p) }
func (s *stream) CloseWrite() error           { return s.resp.CloseWrite() }
func (s *stream) Close() error {
	var err error
	s.once.Do(func() {
		err = s.resp.Conn.Close()
		err2 := s.outClose.Close()
		if err == nil {
			err = err2
		}
	})
	return err
}

func (s *stream) Wait() (int, error) {
	<-s.done
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	exitCode, err := s.waitExit(ctx)
	if err != nil {
		return -1, err
	}
	s.mu.Lock()
	copyErr := s.copyErr
	s.mu.Unlock()
	if copyErr != nil && !errors.Is(copyErr, io.ErrClosedPipe) {
		return exitCode, fmt.Errorf("docker: read exec output: %w", copyErr)
	}
	return exitCode, nil
}

func (s *stream) waitExit(ctx context.Context) (int, error) {
	ticker := time.NewTicker(10 * time.Millisecond)
	defer ticker.Stop()
	for {
		result, err := s.client.ExecInspect(ctx, s.execID, client.ExecInspectOptions{})
		if err != nil {
			return -1, fmt.Errorf("docker: inspect exec %s: %w", s.execID, err)
		}
		if !result.Running {
			return result.ExitCode, nil
		}
		select {
		case <-ctx.Done():
			return -1, fmt.Errorf("docker: wait for exec %s: %w", s.execID, ctx.Err())
		case <-ticker.C:
		}
	}
}
