package modal

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"sync"
	"time"

	sdk "github.com/modal-labs/modal-client/go"

	"github.com/gosuda/dalgona/internal/runner"
)

type sdkAPI struct {
	client *sdk.Client
}

func NewFromEnv(bootstrap runner.BootstrapFunc) (*Service, error) {
	return NewFromEnvWithImage(defaultImage, bootstrap)
}

func NewFromEnvWithImage(image string, bootstrap runner.BootstrapFunc) (*Service, error) {
	client, err := sdk.NewClient()
	if err != nil {
		return nil, fmt.Errorf("modal: create client: %w", err)
	}
	return NewWithImage(&sdkAPI{client: client}, image, bootstrap), nil
}

func (a *sdkAPI) Create(ctx context.Context, request CreateRequest) (Box, string, error) {
	if request.Name == "" || request.Sequence < 1 {
		return nil, "", errors.New("modal: invalid create request")
	}
	app, err := a.client.Apps.FromName(ctx, "dalgona", &sdk.AppFromNameParams{CreateIfMissing: true})
	if err != nil {
		return nil, "", fmt.Errorf("get app: %w", err)
	}
	volume, err := a.client.Volumes.FromName(ctx, request.Volume, &sdk.VolumeFromNameParams{CreateIfMissing: true, Version: 2})
	if err != nil {
		return nil, "", fmt.Errorf("get volume: %w", err)
	}
	image := a.client.Images.FromRegistry(request.Image, nil)
	params := &sdk.SandboxCreateParams{
		Timeout:     request.Timeout,
		IdleTimeout: request.IdleTimeout,
		Volumes:     map[string]*sdk.Volume{"/data": volume},
		Env: map[string]string{
			"HOME":           "/data/home",
			"DALGONA_SYNCFS": "/data",
		},
		Command: []string{"sleep", "infinity"},
	}
	var createErr error
	for sequence := request.Sequence; sequence < request.Sequence+100; sequence++ {
		params.Name = fmt.Sprintf("%s-%d", request.Name, sequence)
		var box *sdk.Sandbox
		box, createErr = a.client.Sandboxes.Create(ctx, app, image, params)
		if createErr == nil {
			return finishCreate(ctx, box)
		}
		if _, ok := errors.AsType[sdk.AlreadyExistsError](createErr); !ok {
			return nil, "", fmt.Errorf("create sandbox: %w", createErr)
		}
	}
	return nil, "", fmt.Errorf("create sandbox after 100 name collisions: %w", createErr)
}

func finishCreate(ctx context.Context, box *sdk.Sandbox) (Box, string, error) {
	if err := initializeWorkspace(ctx, box); err != nil {
		cleanupCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), time.Minute)
		defer cancel()
		_, terminateErr := box.Terminate(cleanupCtx, &sdk.SandboxTerminateParams{Wait: true})
		if terminateErr != nil {
			return nil, "", errors.Join(err, fmt.Errorf("modal: terminate failed sandbox: %w", terminateErr))
		}
		return nil, "", err
	}
	return &sdkBox{box: box}, box.SandboxID, nil
}

func initializeWorkspace(ctx context.Context, box *sdk.Sandbox) error {
	process, err := box.Exec(ctx, []string{"sh", "-c", "mkdir -p /data/workspace /data/home && ln -sfn /data/workspace /workspace"}, &sdk.SandboxExecParams{
		Stdout:  sdk.Pipe,
		Stderr:  sdk.Pipe,
		Workdir: "/",
	})
	if err != nil {
		return fmt.Errorf("modal: initialize workspace: %w", err)
	}
	stream := newSDKProcess(process)
	output, readErr := io.ReadAll(stream)
	code, waitErr := stream.Wait()
	closeErr := stream.Close()
	if err := errors.Join(readErr, waitErr, closeErr); err != nil {
		return fmt.Errorf("modal: initialize workspace: %w", err)
	}
	if code != 0 {
		return fmt.Errorf("modal: initialize workspace exited %d: %s", code, output)
	}
	return nil
}

func (a *sdkAPI) FromID(ctx context.Context, id string) (Box, error) {
	box, err := a.client.Sandboxes.FromID(ctx, id, nil)
	if err != nil {
		return nil, fmt.Errorf("get sandbox: %w", err)
	}
	return &sdkBox{box: box}, nil
}

func (a *sdkAPI) DeleteVolume(ctx context.Context, name string) error {
	if err := a.client.Volumes.Delete(ctx, name, &sdk.VolumeDeleteParams{AllowMissing: true}); err != nil {
		return fmt.Errorf("delete volume: %w", err)
	}
	return nil
}

type sdkBox struct {
	box *sdk.Sandbox
}

func (b *sdkBox) Exec(ctx context.Context, argv []string) (Process, error) {
	process, err := b.box.Exec(ctx, argv, &sdk.SandboxExecParams{
		Stdout:  sdk.Pipe,
		Stderr:  sdk.Pipe,
		Workdir: "/workspace",
	})
	if err != nil {
		return nil, fmt.Errorf("exec: %w", err)
	}
	return newSDKProcess(process), nil
}

func (b *sdkBox) Poll(ctx context.Context) (*int, error) {
	exit, err := b.box.Poll(ctx, nil)
	if err != nil {
		return nil, fmt.Errorf("poll: %w", err)
	}
	return exit, nil
}

func (b *sdkBox) Terminate(ctx context.Context) error {
	if _, err := b.box.Terminate(ctx, &sdk.SandboxTerminateParams{Wait: true}); err != nil {
		return fmt.Errorf("terminate: %w", err)
	}
	return nil
}

type sdkProcess struct {
	process   *sdk.ContainerProcess
	done      chan struct{}
	closeOnce sync.Once
	closeErr  error
}

func newSDKProcess(process *sdk.ContainerProcess) *sdkProcess {
	go func() {
		if _, err := io.Copy(io.Discard, process.Stderr); err != nil && !errors.Is(err, io.ErrClosedPipe) {
			slog.Default().Debug("modal: drain process stderr", "error", err)
		}
	}()
	return &sdkProcess{process: process, done: make(chan struct{})}
}

func (p *sdkProcess) Read(data []byte) (int, error) {
	n, err := p.process.Stdout.Read(data)
	if errors.Is(err, io.EOF) {
		return n, io.EOF
	}
	if err != nil {
		return n, fmt.Errorf("modal: read process output: %w", err)
	}
	return n, nil
}

func (p *sdkProcess) Write(data []byte) (int, error) {
	n, err := p.process.Stdin.Write(data)
	if err != nil {
		return n, fmt.Errorf("modal: write process input: %w", err)
	}
	return n, nil
}

func (p *sdkProcess) CloseWrite() error { return closePart("stdin", p.process.Stdin) }

func (p *sdkProcess) Close() error {
	p.closeOnce.Do(func() {
		close(p.done)
		p.closeErr = errors.Join(
			closePart("stdin", p.process.Stdin),
			closePart("stdout", p.process.Stdout),
			closePart("stderr", p.process.Stderr),
		)
	})
	return p.closeErr
}

func closePart(name string, closer io.Closer) error {
	if err := closer.Close(); err != nil {
		return fmt.Errorf("modal: close process %s: %w", name, err)
	}
	return nil
}

func (p *sdkProcess) Wait() (int, error) {
	ctx, cancel := context.WithTimeoutCause(context.Background(), 25*time.Hour, errors.New("modal: process wait exceeded 25 hours"))
	defer cancel()
	go func() {
		select {
		case <-p.done:
			cancel()
		case <-ctx.Done():
		}
	}()
	code, err := p.process.Wait(ctx, nil)
	if err != nil {
		return 0, fmt.Errorf("modal: wait for process: %w", err)
	}
	return code, nil
}
