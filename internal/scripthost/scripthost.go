package scripthost

import (
	"context"
	"log/slog"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/script"
	"github.com/gosuda/dalgona/internal/tool"
)

type JudgeFactory func(ctx context.Context, model, threadModel string) (decide.Client, error)

type Config struct {
	Judge  JudgeFactory
	Mode   config.ToolMode
	Eval   config.EvalConfig
	Logger *slog.Logger
}

func NewEffects(t *agent.Thread, cfg Config) *effects {
	if cfg.Logger == nil {
		cfg.Logger = slog.Default()
	}
	return &effects{t: t, judge: cfg.Judge, log: cfg.Logger, idx: newEffectCache()}
}

func New(cfg Config) agent.ExtensionFactory {
	return func(t *agent.Thread) agent.Extension {
		return extension{mode: cfg.Mode, eval: cfg.Eval, fx: *NewEffects(t, cfg)}
	}
}

type extension struct {
	mode config.ToolMode
	eval config.EvalConfig
	fx   effects
}

func (e extension) Tools(builtin []tool.Tool) []tool.Tool {
	specs := make([]tool.Spec, len(builtin))
	for i, item := range builtin {
		specs[i] = item.Spec()
	}
	return append(builtin, script.NewEval(e.fx, script.EvalOptions{Mode: e.mode, Tools: specs, Config: e.eval}))
}

func (e extension) Visible(specs []tool.Spec) []tool.Spec { return script.ModelTools(e.mode, specs) }

func (e extension) Direct(name string) error { return script.CheckDirect(e.mode, name) }

func (e extension) ToolCall(context.Context, tool.Call) (agent.HookVerdict, error) {
	return agent.HookVerdict{}, nil
}

func (e extension) TurnEnd(context.Context, string, string) error { return nil }
