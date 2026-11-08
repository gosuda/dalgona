package app

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/model"
)

const (
	defaultDecisionsDecider = "decisions:typesafe/jev-1.14"
	defaultLLMDecider       = "llm:smol"
)

type roleResolver struct {
	mu   sync.Mutex
	host *agent.Host
}

func (r *roleResolver) set(host *agent.Host) {
	r.mu.Lock()
	r.host = host
	r.mu.Unlock()
}

func (r *roleResolver) resolve(ctx context.Context, target, threadModel string) (string, error) {
	if strings.Contains(target, "/") {
		return target, nil
	}
	r.mu.Lock()
	host := r.host
	r.mu.Unlock()
	if host == nil {
		return "", errors.New("decider: host is not ready")
	}
	return host.ResolveRole(ctx, target, threadModel)
}

func (r *roleResolver) threadModel(threadID, fallback string) string {
	r.mu.Lock()
	host := r.host
	r.mu.Unlock()
	if host == nil {
		return fallback
	}
	for _, info := range host.Threads() {
		if info.ID == threadID && info.Model != "" {
			return info.Model
		}
	}
	return fallback
}

type judges struct {
	cfg   config.Config
	creds *credentials
	roles *roleResolver
	spec  decide.Spec
}

func newJudges(ctx context.Context, cfg config.Config, creds *credentials, roles *roleResolver) (*judges, error) {
	value := cfg.ApprovalDecider.Decider
	if value == "" {
		value = defaultLLMDecider
		if _, err := creds.stored(ctx, "openrouter", ""); err == nil {
			value = defaultDecisionsDecider
		}
	}
	spec, err := decide.Parse(value)
	if err != nil {
		return nil, fmt.Errorf("config: [approval] decider: %w", err)
	}
	return &judges{cfg: cfg, creds: creds, roles: roles, spec: spec}, nil
}

func (j *judges) decider() agent.Decider { return threadDecider{j: j} }

func (j *judges) judge(ctx context.Context, modelSpec, threadModel string) (decide.Client, error) {
	spec := j.spec
	if modelSpec != "" {
		parsed, err := decide.Parse(modelSpec)
		if err != nil {
			parsed = decide.Spec{Adapter: decide.AdapterLLM, Model: modelSpec}
		}
		spec = parsed
	}
	client, _, err := j.client(ctx, spec, threadModel)
	return client, err
}

func (j *judges) client(ctx context.Context, spec decide.Spec, threadModel string) (decide.Client, string, error) {
	if spec.Adapter == decide.AdapterDecisions {
		return decide.NewDecisions(decide.DecisionsConfig{Model: spec.Model, Token: j.creds.apiKey("openrouter", "")}), spec.Model, nil
	}
	resolved, err := j.roles.resolve(ctx, spec.Model, threadModel)
	if err != nil {
		return nil, "", fmt.Errorf("judge model %s: %w", spec.Model, err)
	}
	providerID, modelName, err := model.ParseModelID(resolved)
	if err != nil {
		return nil, "", fmt.Errorf("judge model %s: %w", resolved, err)
	}
	registry, err := j.creds.registry(j.cfg, nil)
	if err != nil {
		return nil, "", err
	}
	provider, ok := registry.Provider(providerID)
	if !ok {
		return nil, "", fmt.Errorf("judge model %s: unknown provider %s", resolved, providerID)
	}
	return decide.NewLLM(provider, modelName), resolved, nil
}

type threadDecider struct{ j *judges }

func (d threadDecider) Decide(ctx context.Context, a agent.Action) (agent.Decision, error) {
	client, name, err := d.j.client(ctx, d.j.spec, d.j.roles.threadModel(a.Thread, d.j.cfg.Model))
	if err != nil {
		return agent.Decision{}, err
	}
	return agent.NewDecider(client, name, d.j.cfg.ApprovalDecider.Threshold, d.j.cfg.Product).Decide(ctx, a)
}
