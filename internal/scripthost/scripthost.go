package scripthost

import (
	"bytes"
	"context"
	"log/slog"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/script"
	"github.com/gosuda/dalgona/internal/tool"
)

type JudgeFactory func(ctx context.Context, model, threadModel string) (decide.Client, error)

type Config struct {
	Plugins *script.Plugins
	Judge   JudgeFactory
	Mode    config.ToolMode
	Eval    config.EvalConfig
	Logger  *slog.Logger
}

func New(cfg Config) agent.ExtensionFactory {
	if cfg.Logger == nil {
		cfg.Logger = slog.Default()
	}
	return func(t *agent.Thread) agent.Extension {
		return extension{mode: cfg.Mode, eval: cfg.Eval, plugins: cfg.Plugins, fx: effects{t: t, judge: cfg.Judge, log: cfg.Logger, idx: newEffectCache()}}
	}
}

type extension struct {
	mode    config.ToolMode
	eval    config.EvalConfig
	plugins *script.Plugins
	fx      effects
}

func (e extension) Tools(builtin []tool.Tool) []tool.Tool {
	tools := builtin
	if e.plugins != nil {
		tools = append(tools, e.plugins.Tools(e.fx)...)
	}
	specs := make([]tool.Spec, len(tools))
	for i, item := range tools {
		specs[i] = item.Spec()
	}
	return append(tools, script.NewEval(e.fx, script.EvalOptions{Mode: e.mode, Tools: specs, Config: e.eval}))
}

func (e extension) Visible(specs []tool.Spec) []tool.Spec { return script.ModelTools(e.mode, specs) }

func (e extension) Direct(name string) error { return script.CheckDirect(e.mode, name) }

func (e extension) ToolCall(ctx context.Context, call tool.Call) (agent.HookVerdict, error) {
	if e.plugins == nil {
		return agent.HookVerdict{}, nil
	}
	decision, err := e.plugins.ToolCall(ctx, e.fx, call.ID, call)
	return agent.HookVerdict{Decision: decision.Decision, Reason: decision.Reason, By: decision.By}, err
}

func (e extension) TurnEnd(ctx context.Context, turnID, status string) error {
	if e.plugins == nil {
		return nil
	}
	var payload bytes.Buffer
	encoder := model.NewJSONEncoder(&payload)
	encoder.BeginObject()
	encoder.StringKey("thread", e.fx.t.ID())
	encoder.StringKey("turn", turnID)
	encoder.StringKey("status", status)
	encoder.EndObject()
	if err := encoder.Err(); err != nil {
		return err
	}
	return e.plugins.Emit(ctx, e.fx, turnID+"/turn_end", script.EventTurnEnd, bytes.TrimSpace(payload.Bytes()))
}
