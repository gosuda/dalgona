package registry

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"sync"

	sandbox0sdk "github.com/sandbox0-ai/sdk-go"

	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/cloudflare"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/docker"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/modal"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/sandbox0"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/sprites"
)

type BuildFunc func(ctx context.Context, spec config.RunnerSpec) (sandboxsvc.Service, error)

type Options struct {
	Auth   *auth.Store
	Logger *slog.Logger
	Build  BuildFunc
}

type Registry struct {
	auth     *auth.Store
	log      *slog.Logger
	build    BuildFunc
	mu       sync.Mutex
	services map[config.RunnerSpec]sandboxsvc.Service
	calls    map[config.RunnerSpec]*call
}

type call struct {
	done chan struct{}
	svc  sandboxsvc.Service
	err  error
}

var errBuilderCancelledWhileWaiterLive = errors.New("remote: builder cancelled while a waiter stayed live")

func New(opts Options) *Registry {
	r := &Registry{
		auth: opts.Auth, log: cmp.Or(opts.Logger, slog.Default()),
		services: map[config.RunnerSpec]sandboxsvc.Service{}, calls: map[config.RunnerSpec]*call{},
	}
	r.build = r.construct
	if opts.Build != nil {
		r.build = opts.Build
	}
	return r
}

func (r *Registry) Service(ctx context.Context, spec config.RunnerSpec) (sandboxsvc.Service, error) {
	for {
		r.mu.Lock()
		if svc, ok := r.services[spec]; ok {
			r.mu.Unlock()
			return svc, nil
		}
		if pending, ok := r.calls[spec]; ok {
			r.mu.Unlock()
			svc, err := join(ctx, spec, pending)
			if errors.Is(err, errBuilderCancelledWhileWaiterLive) {
				continue
			}
			return svc, err
		}
		pending := &call{done: make(chan struct{})}
		r.calls[spec] = pending
		r.mu.Unlock()

		pending.svc, pending.err = r.build(ctx, spec)

		r.mu.Lock()
		if pending.err == nil {
			r.services[spec] = pending.svc
		}
		delete(r.calls, spec)
		close(pending.done)
		r.mu.Unlock()
		return pending.svc, pending.err
	}
}

func join(ctx context.Context, spec config.RunnerSpec, pending *call) (sandboxsvc.Service, error) {
	select {
	case <-pending.done:
		if ctx.Err() == nil && buildCancelled(pending.err) {
			return nil, errBuilderCancelledWhileWaiterLive
		}
		return pending.svc, pending.err
	case <-ctx.Done():
		return nil, fmt.Errorf("remote: wait for %s service: %w", spec.Service, ctx.Err())
	}
}

func buildCancelled(err error) bool {
	return err != nil && (errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded))
}

func (r *Registry) construct(ctx context.Context, spec config.RunnerSpec) (sandboxsvc.Service, error) {
	switch spec.Service {
	case "docker":
		return docker.NewFromEnv(runner.Bootstrap)
	case "sprites":
		token, err := r.token("sprites", "SPRITES_TOKEN")
		if err != nil {
			return nil, err
		}
		return sprites.NewFromToken(token, runner.Bootstrap), nil
	case "modal":
		if spec.Image != "" {
			return modal.NewFromEnvWithImageCtx(ctx, spec.Image, runner.Bootstrap)
		}
		return modal.NewFromEnvCtx(ctx, runner.Bootstrap)
	case "sandbox0":
		return r.sandbox0()
	case "cloudflare":
		return r.cloudflare(spec)
	default:
		return nil, fmt.Errorf("remote: unknown service %q", spec.Service)
	}
}

func (r *Registry) sandbox0() (sandboxsvc.Service, error) {
	token, err := r.token("sandbox0", "SANDBOX0_TOKEN")
	if err != nil {
		return nil, err
	}
	opts := []sandbox0sdk.Option{sandbox0sdk.WithToken(token)}
	if base := os.Getenv("SANDBOX0_BASE_URL"); base != "" {
		opts = append(opts, sandbox0sdk.WithBaseURL(base))
	}
	client, err := sandbox0sdk.NewClient(opts...)
	if err != nil {
		return nil, fmt.Errorf("sandbox0: create client: %w", err)
	}
	return sandbox0.New(client, runner.Bootstrap, r.log), nil
}

func (r *Registry) cloudflare(spec config.RunnerSpec) (sandboxsvc.Service, error) {
	if spec.URL == "" {
		return nil, errors.New("remote: cloudflare runner needs url")
	}
	token, err := r.token("cloudflare", "DALGONA_CLOUDFLARE_TOKEN")
	if err != nil {
		return nil, err
	}
	return cloudflare.New(spec.URL, token), nil
}

func (r *Registry) token(provider, env string) (string, error) {
	if key := os.Getenv(env); key != "" {
		return key, nil
	}
	if key := r.storedKey(provider); key != "" {
		return key, nil
	}
	return "", fmt.Errorf("remote: %s needs %s or `dalgona login %s --api-key`", provider, env, provider)
}

func (r *Registry) storedKey(provider string) string {
	if r.auth == nil {
		return ""
	}
	for _, entry := range r.auth.Entries() {
		if entry.ID == provider {
			return entry.Credential.Key
		}
	}
	return ""
}
