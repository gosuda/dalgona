package app

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"strings"
	"sync"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/decide"
	"github.com/gosuda/dalgona/internal/dal/model"
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
	cfg       config.Config
	creds     *credentials
	roles     *roleResolver
	spec      decide.Spec
	cooldowns *decide.Cooldowns

	fallbackLogOnce sync.Once
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
	return &judges{cfg: cfg, creds: creds, roles: roles, spec: spec, cooldowns: decide.NewCooldowns(nil)}, nil
}

func (j *judges) decider() agent.Decider {
	return agent.NewFactoryDecider(j.resolve, j.cfg.ApprovalDecider.Threshold, j.cfg.Product)
}

func (j *judges) resolve(ctx context.Context, threadID string) (decide.Client, string, error) {
	return j.client(ctx, j.spec, j.roles.threadModel(threadID, j.cfg.Model))
}

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
	primary := lazyJudgeClient{j: j, spec: spec, threadModel: threadModel}
	smolSpec := decide.Spec{Adapter: decide.AdapterLLM, Model: "smol"}
	smol := lazyJudgeClient{j: j, spec: smolSpec, threadModel: threadModel, smol: true}
	model := j.modelName(ctx, spec, threadModel)
	client := decide.WithFallback(
		decide.Named{Client: primary, Model: spec.Adapter + ":" + model},
		decide.Named{Client: smol, Model: decide.AdapterLLM + ":" + j.modelName(ctx, smolSpec, threadModel)},
		j.cooldowns,
	)
	return client, model, nil
}

func (j *judges) modelName(ctx context.Context, spec decide.Spec, threadModel string) string {
	if spec.Adapter == decide.AdapterLLM {
		if resolved, err := j.roles.resolve(ctx, spec.Model, threadModel); err == nil {
			return resolved
		}
	}
	return spec.Model
}

type lazyJudgeClient struct {
	j           *judges
	spec        decide.Spec
	threadModel string
	smol        bool
}

func (l lazyJudgeClient) Ask(ctx context.Context, state jsontext.Value, questions map[string]decide.Question) (decide.Result, error) {
	client, _, err := l.j.buildClient(ctx, l.spec, l.threadModel)
	if err != nil {
		if l.smol {
			l.j.logFallbackUnavailable(err)
		}
		return decide.Result{}, err
	}
	return client.Ask(ctx, state, questions)
}

func (j *judges) logFallbackUnavailable(err error) {
	j.fallbackLogOnce.Do(func() {
		j.creds.log.Error("judge fallback unavailable: " + j.creds.store.Mask(err.Error()))
	})
}

func (j *judges) buildClient(ctx context.Context, spec decide.Spec, threadModel string) (decide.Client, string, error) {
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
	return decide.NewLLM(resolved, provider, modelName), resolved, nil
}
