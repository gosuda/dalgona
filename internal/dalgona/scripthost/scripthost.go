package scripthost

import (
	"bytes"
	"context"
	"log/slog"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/model"
	corescript "github.com/gosuda/dalgona/internal/script"
	corescripthost "github.com/gosuda/dalgona/internal/scripthost"
	"github.com/gosuda/dalgona/internal/tool"

	"github.com/gosuda/dalgona/internal/dalgona/script"
)

type Config struct {
	Core        corescripthost.Config
	Plugins     *script.Plugins
	HookTimeout time.Duration
}

func New(cfg Config) agent.ExtensionFactory {
	if cfg.Core.Logger == nil {
		cfg.Core.Logger = slog.Default()
	}
	plugins := cfg.Plugins
	if plugins != nil && cfg.HookTimeout > 0 {
		plugins = plugins.WithHookTimeout(cfg.HookTimeout)
	}
	return func(t *agent.Thread) agent.Extension {
		return extension{thread: t, mode: cfg.Core.Mode, eval: cfg.Core.Eval, plugins: plugins, fx: corescripthost.NewEffects(t, cfg.Core)}
	}
}

type extension struct {
	thread  *agent.Thread
	mode    config.ToolMode
	eval    config.EvalConfig
	plugins *script.Plugins
	fx      corescript.Effects
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
	return append(tools, corescript.NewEval(e.fx, corescript.EvalOptions{Mode: e.mode, Tools: specs, Config: e.eval}))
}

func (e extension) Visible(specs []tool.Spec) []tool.Spec {
	return corescript.ModelTools(e.mode, specs)
}

func (e extension) Direct(name string) error { return corescript.CheckDirect(e.mode, name) }

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
	encoder.StringKey("thread", e.thread.ID())
	encoder.StringKey("turn", turnID)
	encoder.StringKey("status", status)
	encoder.EndObject()
	if err := encoder.Err(); err != nil {
		return err
	}
	return e.plugins.Emit(ctx, e.fx, turnID+"/turn_end", script.EventTurnEnd, bytes.TrimSpace(payload.Bytes()))
}
