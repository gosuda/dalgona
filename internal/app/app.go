package app

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"log/slog"
	"path/filepath"
	"sync"
	"sync/atomic"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/advisor"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/journal"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/exec"
	"github.com/gosuda/dalgona/internal/tool/patch"
	"github.com/gosuda/dalgona/internal/tool/read"
	"github.com/gosuda/dalgona/internal/tool/search"
	"github.com/gosuda/dalgona/internal/tool/task"
	"github.com/gosuda/dalgona/internal/tool/todo"
	"github.com/gosuda/dalgona/internal/tool/webfetch"
)

type Options struct {
	Product   config.Product
	Cwd       string
	Flags     config.Flags
	Config    *config.Config
	Logger    *slog.Logger
	Advisors  *advisor.Registry
	Batteries func(context.Context, Base) (Batteries, error)
}

type Base struct {
	Config  config.Config
	Journal *journal.Store
	Auth    *auth.Store
	Local   agent.RunnerFactory
	Judge   func(ctx context.Context, model, threadModel string) (decide.Client, error)
	Logger  *slog.Logger
}

type Batteries struct {
	Tools     func(ctx context.Context, cwd string) []tool.Tool
	Skills    agent.SkillSet
	Extension agent.ExtensionFactory
	Runner    agent.RunnerFactory
	Attach    func(*agent.Host)
	Close     func() error
}

func NewHost(ctx context.Context, opts Options) (*agent.Host, func() error, error) {
	if err := ctx.Err(); err != nil {
		return nil, nil, fmt.Errorf("app: %w", err)
	}
	cfg, err := loadConfig(opts)
	if err != nil {
		return nil, nil, err
	}
	store, err := auth.NewWithPatterns(cfg.Home, cfg.Secrets.Patterns)
	if err != nil {
		return nil, nil, err
	}
	product := cmp.Or(opts.Product, cfg.Product)
	creds, roles := newCredentials(store, product, opts.Logger), &roleResolver{}
	judges, err := newJudges(ctx, cfg, creds, roles)
	if err != nil {
		return nil, nil, err
	}
	journalStore, err := openJournal(product, cfg.Home)
	if err != nil {
		return nil, nil, err
	}
	base := Base{Config: cfg, Journal: journalStore, Auth: store, Local: runnerFactory(cfg.Home), Judge: judges.judge, Logger: opts.Logger}
	extra, err := loadBatteries(ctx, opts, base)
	if err != nil {
		return nil, nil, errors.Join(err, journalStore.Close())
	}
	advisors := newAdvisors(opts.Advisors, opts.Logger)
	release := func() error { return errors.Join(extra.Close(), journalStore.Close()) }
	host, err := agent.NewHost(agent.HostConfig{
		Journal: journalStore, Config: cfg, Auth: store, Tools: toolSet(cfg, extra.Tools),
		Providers:   func(onRetry agent.RetryFunc) (*model.Registry, error) { return creds.registry(cfg, onRetry) },
		ProviderIDs: providerIDs(cfg), Decider: judges.decider(), Extension: extra.Extension, EditFeedback: cfg.EditReview.Feedback,
		Runner: extra.Runner, Product: product, Logger: opts.Logger, Skills: extra.Skills,
		OnChildStarted: advisors.attach,
	})
	if err != nil {
		return nil, nil, errors.Join(err, release())
	}
	roles.set(host)
	advisors.host.Store(host)
	extra.Attach(host)
	closeAll := sync.OnceValue(func() error { return errors.Join(advisors.close(), host.Close(), release()) })
	return host, closeAll, nil
}

func loadConfig(opts Options) (config.Config, error) {
	if opts.Config != nil {
		return *opts.Config, nil
	}
	return config.Load(opts.Product, opts.Cwd, opts.Flags)
}

func openJournal(product config.Product, home string) (*journal.Store, error) {
	if product == config.ProductDal {
		return journal.OpenShared(home)
	}
	return journal.Open(home)
}

func loadBatteries(ctx context.Context, opts Options, base Base) (Batteries, error) {
	extra := Batteries{}
	if opts.Batteries != nil {
		loaded, err := opts.Batteries(ctx, base)
		if err != nil {
			return Batteries{}, err
		}
		extra = loaded
	}
	return withDefaults(extra, base.Local), nil
}

func withDefaults(extra Batteries, local agent.RunnerFactory) Batteries {
	if extra.Tools == nil {
		extra.Tools = func(context.Context, string) []tool.Tool { return nil }
	}
	if extra.Runner == nil {
		extra.Runner = local
	}
	if extra.Attach == nil {
		extra.Attach = func(*agent.Host) {}
	}
	if extra.Close == nil {
		extra.Close = func() error { return nil }
	}
	return extra
}

func toolSet(cfg config.Config, extra func(ctx context.Context, cwd string) []tool.Tool) agent.ToolSet {
	return func(ctx context.Context, modelID, cwd string) ([]tool.Tool, error) {
		tools := []tool.Tool{
			read.New(), search.New(), patch.New(cfg.EditStyle.EditStyle(modelID)),
			exec.New(), todo.New(), webfetch.New(),
		}
		return append(tools, extra(ctx, cwd)...), nil
	}
}

func runnerFactory(home string) agent.RunnerFactory {
	return func(_ context.Context, threadID, name string) (runner.Runner, error) {
		if name != "" && name != "local" {
			return nil, fmt.Errorf("runner %q is not available in this build", name)
		}
		return runner.NewLocal(filepath.Join(home, "sessions", threadID, "runs"))
	}
}

type advisors struct {
	registry *advisor.Registry
	log      *slog.Logger
	host     atomic.Pointer[agent.Host]
}

func newAdvisors(registry *advisor.Registry, logger *slog.Logger) *advisors {
	if registry == nil {
		registry = advisor.NewRegistry()
	}
	return &advisors{registry: registry, log: cmp.Or(logger, slog.Default())}
}

func (a *advisors) attach(child *agent.Thread, req task.Request) {
	host := a.host.Load()
	if host == nil {
		return
	}
	attach := a.registry.Attach
	if req.Advisor {
		attach = a.registry.AttachChild
	}
	if _, err := attach(context.Background(), host, child); err != nil {
		a.log.Error("advisor: attach", "thread", child.ID(), "error", err)
	}
}

func (a *advisors) close() error { return a.registry.Close() }
