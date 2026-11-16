package script

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"time"

	"go.starlark.net/starlark"

	corescript "github.com/gosuda/dalgona/internal/script"
	"github.com/gosuda/dalgona/internal/tool"
)

const (
	kindTool          = "tool"
	kindCommand       = "command"
	kindHook          = "on"
	EventSessionStart = "session_start"
	EventToolCall     = "tool_call"
	EventTurnEnd      = "turn_end"
	DecisionAllow     = "allow"
	DecisionDeny      = "deny"
)

const HookTimeout = 5 * time.Second

var errHookTimeout = errors.New("plugin hook timed out")

type registration struct {
	kind        string
	name        string
	description string
	params      jsontext.Value
	fn          string
	impl        starlark.Callable
}

type pluginFile struct {
	name   string
	source []byte
	regs   []registration
}

type Plugins struct {
	files       []*pluginFile
	hookTimeout time.Duration
}

type Command struct {
	Name        string
	Description string
}

type CommandCall struct {
	RunID string
	Name  string
	Args  string
}
type HookDecision struct {
	Decision string
	Reason   string
	By       string
}

type invocation struct {
	runID string
	file  *pluginFile
	reg   registration
	args  jsontext.Value
	call  func(ctx context.Context, r *corescript.Runtime, impl starlark.Callable) (starlark.Value, error)
}

func LoadPlugins(home string) (*Plugins, error) {
	paths, err := filepath.Glob(filepath.Join(home, "plugins", "*.star"))
	if err != nil {
		return nil, fmt.Errorf("list plugins: %w", err)
	}
	p := &Plugins{}
	var errs []error
	for _, path := range paths {
		if err := p.loadFile(path); err != nil {
			errs = append(errs, fmt.Errorf("plugin %s: %w", filepath.Base(path), err))
		}
	}
	return p, errors.Join(errs...)
}

func (p *Plugins) WithHookTimeout(d time.Duration) *Plugins {
	scoped := *p
	scoped.hookTimeout = d
	return &scoped
}

func (p *Plugins) timeout() time.Duration {
	if p.hookTimeout > 0 {
		return p.hookTimeout
	}
	return HookTimeout
}

func (p *Plugins) loadFile(path string) error {
	src, err := os.ReadFile(path)
	if err != nil {
		return fmt.Errorf("read: %w", err)
	}
	name := filepath.Base(path)
	r, env, end := corescript.NewRuntime(context.Background(), nil, "load:"+name, 0)
	defer end()
	regs, err := load(r, name, src, env)
	if err != nil {
		return err
	}
	for _, reg := range regs {
		if p.registered(reg) {
			return fmt.Errorf("%s %s is already registered", reg.kind, reg.name)
		}
	}
	p.files = append(p.files, &pluginFile{name: name, source: src, regs: regs})
	return nil
}

func (p *Plugins) registered(reg registration) bool {
	if reg.kind == kindHook {
		return false
	}
	for _, file := range p.files {
		if slices.ContainsFunc(file.regs, func(other registration) bool { return other.kind == reg.kind && other.name == reg.name }) {
			return true
		}
	}
	return false
}

func load(r *corescript.Runtime, name string, src []byte, env starlark.StringDict) ([]registration, error) {
	var regs []registration
	add := func(reg registration) { regs = append(regs, reg) }
	env["tool"] = starlark.NewBuiltin("tool", registerTool(add))
	env["command"] = starlark.NewBuiltin("command", registerCommand(add))
	env["on"] = starlark.NewBuiltin("on", registerHook(add))
	_, err := r.LoadCell(context.Background(), name, src, env)
	if err != nil {
		return nil, err
	}
	return regs, nil
}

type builtinFunc = func(*starlark.Thread, *starlark.Builtin, starlark.Tuple, []starlark.Tuple) (starlark.Value, error)

func registerTool(add func(registration)) builtinFunc {
	return func(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
		var name, description string
		var params *starlark.Dict
		var impl starlark.Callable
		if err := starlark.UnpackArgs(b.Name(), args, kwargs, "name", &name, "description", &description, "params", &params, "impl", &impl); err != nil {
			return nil, err
		}
		schema, err := corescript.ToJSON(params)
		if err != nil {
			return nil, fmt.Errorf("tool %s: params: %w", name, err)
		}
		if name == "" {
			return nil, errors.New("tool: name must not be empty")
		}
		add(registration{kind: kindTool, name: name, description: description, params: schema, fn: impl.Name(), impl: impl})
		return starlark.None, nil
	}
}

func registerCommand(add func(registration)) builtinFunc {
	return func(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
		var name, description string
		var impl starlark.Callable
		if err := starlark.UnpackArgs(b.Name(), args, kwargs, "name", &name, "description", &description, "impl", &impl); err != nil {
			return nil, err
		}
		if name == "" {
			return nil, errors.New("command: name must not be empty")
		}
		add(registration{kind: kindCommand, name: name, description: description, fn: impl.Name(), impl: impl})
		return starlark.None, nil
	}
}

func registerHook(add func(registration)) builtinFunc {
	return func(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
		var event string
		var impl starlark.Callable
		if err := starlark.UnpackArgs(b.Name(), args, kwargs, "event", &event, "impl", &impl); err != nil {
			return nil, err
		}
		if event != EventSessionStart && event != EventToolCall && event != EventTurnEnd {
			return nil, fmt.Errorf("on: event must be %q, %q, or %q, got %q", EventSessionStart, EventToolCall, EventTurnEnd, event)
		}
		add(registration{kind: kindHook, name: event, fn: impl.Name(), impl: impl})
		return starlark.None, nil
	}
}

func (p *Plugins) invoke(ctx context.Context, fx corescript.Effects, inv invocation) (corescript.Outcome, error) {
	start, done, err := fx.Started(corescript.RunStart{ID: inv.runID, Entry: "plugins/" + inv.file.name, Source: inv.file.source, Args: inv.args, Seed: corescript.NewSeed()})
	if err != nil {
		return corescript.Outcome{}, fmt.Errorf("plugin run %s: start run: %w", inv.runID, err)
	}
	if done != nil {
		return *done, nil
	}
	return corescript.Complete(context.WithoutCancel(ctx), fx, start.ID, inv.run(ctx, fx, start))
}

func (inv invocation) run(ctx context.Context, fx corescript.Effects, start corescript.RunStart) corescript.Outcome {
	r, env, end := corescript.NewRuntime(ctx, fx, start.ID, start.Seed)
	defer end()
	regs, err := load(r, inv.file.name, start.Source, env)
	if err != nil {
		return r.Finish(ctx, nil, err)
	}
	i := slices.IndexFunc(regs, func(reg registration) bool {
		return reg.kind == inv.reg.kind && reg.name == inv.reg.name && reg.fn == inv.reg.fn
	})
	if i < 0 {
		return corescript.Outcome{Status: corescript.StatusFailed, Error: corescript.Failure{Code: "error", Message: fmt.Sprintf("plugin %s no longer registers %s %s", inv.file.name, inv.reg.kind, inv.reg.name), Stage: "run"}}
	}
	value, err := inv.call(ctx, r, regs[i].impl)
	return r.Finish(ctx, value, err)
}

func (p *Plugins) Tools(fx corescript.Effects) []tool.Tool {
	var tools []tool.Tool
	for _, file := range p.files {
		for _, reg := range file.regs {
			if reg.kind == kindTool {
				tools = append(tools, &PluginTool{plugins: p, fx: fx, file: file, reg: reg})
			}
		}
	}
	return tools
}

type PluginTool struct {
	plugins *Plugins
	fx      corescript.Effects
	file    *pluginFile
	reg     registration
}

func (t *PluginTool) Spec() tool.Spec {
	return tool.Spec{Name: t.reg.name, Description: t.reg.description, Schema: t.reg.params}
}

func (t *PluginTool) Class() tool.Class { return tool.ClassScript }

func (t *PluginTool) Run(ctx context.Context, call tool.Call, _ tool.Env) (tool.Result, error) {
	kwargs, err := objectKwargs(call.Args)
	if err != nil {
		return corescript.ReplyResult(call.ID, corescript.Outcome{Status: corescript.StatusFailed, Error: corescript.Failure{Code: "invalid_args", Message: fmt.Sprintf("%s: %v", t.reg.name, err), Stage: "args"}})
	}
	outcome, err := t.plugins.invoke(ctx, t.fx, invocation{runID: call.ID, file: t.file, reg: t.reg, args: call.Args, call: func(ctx context.Context, r *corescript.Runtime, impl starlark.Callable) (starlark.Value, error) {
		return r.Call(ctx, impl, starlark.Tuple{r.ContextValue()}, kwargs)
	}})
	if err != nil {
		return tool.Result{}, err
	}
	return corescript.ReplyResult(call.ID, outcome)
}

func objectKwargs(args jsontext.Value) ([]starlark.Tuple, error) {
	decoded, err := corescript.FromJSON(args)
	if err != nil {
		return nil, err
	}
	if decoded == starlark.None {
		return nil, nil
	}
	dict, ok := decoded.(*starlark.Dict)
	if !ok {
		return nil, errors.New("arguments must be a JSON object")
	}
	return dict.Items(), nil
}

func (p *Plugins) Commands() []Command {
	var commands []Command
	for _, file := range p.files {
		for _, reg := range file.regs {
			if reg.kind == kindCommand {
				commands = append(commands, Command{Name: reg.name, Description: reg.description})
			}
		}
	}
	return commands
}

func (p *Plugins) RunCommand(ctx context.Context, fx corescript.Effects, call CommandCall) (corescript.Outcome, error) {
	file, reg, ok := p.find(kindCommand, call.Name)
	if !ok {
		return corescript.Outcome{}, fmt.Errorf("unknown command %s", call.Name)
	}
	return p.invoke(ctx, fx, invocation{runID: call.RunID, file: file, reg: reg, args: corescript.StringJSON(call.Args), call: func(ctx context.Context, r *corescript.Runtime, impl starlark.Callable) (starlark.Value, error) {
		return r.Call(ctx, impl, starlark.Tuple{r.ContextValue(), starlark.String(call.Args)}, nil)
	}})
}

func (p *Plugins) find(kind, name string) (*pluginFile, registration, bool) {
	for _, file := range p.files {
		for _, reg := range file.regs {
			if reg.kind == kind && reg.name == name {
				return file, reg, true
			}
		}
	}
	return nil, registration{}, false
}

type hookTarget struct {
	file *pluginFile
	reg  registration
}

func (h hookTarget) by() string {
	return "hook:" + h.file.name + ":" + h.reg.fn
}

func (p *Plugins) hooks(event string) []hookTarget {
	var targets []hookTarget
	for _, file := range p.files {
		for _, reg := range file.regs {
			if reg.kind == kindHook && reg.name == event {
				targets = append(targets, hookTarget{file: file, reg: reg})
			}
		}
	}
	return targets
}

func (p *Plugins) hook(ctx context.Context, fx corescript.Effects, runID string, target hookTarget, payload jsontext.Value) (corescript.Outcome, error) {
	event, err := corescript.FromJSON(payload)
	if err != nil {
		return corescript.Outcome{}, fmt.Errorf("%s: payload: %w", target.by(), err)
	}
	return p.invoke(ctx, fx, invocation{runID: runID + "/" + target.by(), file: target.file, reg: target.reg, args: payload, call: func(ctx context.Context, r *corescript.Runtime, impl starlark.Callable) (starlark.Value, error) {
		return r.Call(ctx, impl, starlark.Tuple{r.ContextValue(), event}, nil)
	}})
}

func (h hookTarget) failure(outcome corescript.Outcome, err error) error {
	if err != nil {
		return fmt.Errorf("%s: %w", h.by(), err)
	}
	if outcome.Status != corescript.StatusCompleted {
		return fmt.Errorf("%s: %s: %s", h.by(), outcome.Status, outcome.Error.Message)
	}
	return nil
}

func (h hookTarget) decision(outcome corescript.Outcome, err error) (HookDecision, error) {
	if failure := h.failure(outcome, err); failure != nil {
		return HookDecision{}, failure
	}
	decision, err := parseDecision(outcome.Value)
	if err != nil {
		return HookDecision{}, fmt.Errorf("%s: %w", h.by(), err)
	}
	if decision.Decision != "" {
		decision.By = h.by()
	}
	return decision, nil
}

func (p *Plugins) Emit(ctx context.Context, fx corescript.Effects, runID, event string, payload jsontext.Value) error {
	if event != EventSessionStart && event != EventTurnEnd {
		return fmt.Errorf("emit: event must be %q or %q, got %q", EventSessionStart, EventTurnEnd, event)
	}
	var errs []error
	for _, target := range p.hooks(event) {
		errs = append(errs, target.failure(p.hook(ctx, fx, runID, target, payload)))
	}
	return errors.Join(errs...)
}

func (p *Plugins) ToolCall(ctx context.Context, fx corescript.Effects, runID string, call tool.Call) (HookDecision, error) {
	payload := toolCallPayload(call)
	var allow, deny HookDecision
	var errs []error
	for _, target := range p.hooks(EventToolCall) {
		decision, err := p.toolCallDecision(ctx, fx, runID, target, payload)
		if err != nil {
			errs = append(errs, err)
			continue
		}
		switch decision.Decision {
		case DecisionDeny:
			if deny.Decision == "" {
				deny = decision
			}
		case DecisionAllow:
			if allow.Decision == "" {
				allow = decision
			}
		}
	}
	if deny.Decision != "" {
		return deny, errors.Join(errs...)
	}
	if allow.Decision != "" {
		return allow, errors.Join(errs...)
	}
	return HookDecision{}, errors.Join(errs...)
}

func (p *Plugins) toolCallDecision(ctx context.Context, fx corescript.Effects, runID string, target hookTarget, payload jsontext.Value) (HookDecision, error) {
	limit := p.timeout()
	hookCtx, cancel := context.WithTimeoutCause(ctx, limit, errHookTimeout)
	defer cancel()
	decision, err := target.decision(p.hook(hookCtx, fx, runID, target, payload))
	if err != nil && errors.Is(context.Cause(hookCtx), errHookTimeout) {
		return HookDecision{}, fmt.Errorf("plugin %s hook timed out after %s", target.file.name, limit)
	}
	return decision, err
}

func toolCallPayload(call tool.Call) jsontext.Value {
	args := call.Args
	if len(args) == 0 {
		args = jsontext.Value("null")
	}
	out := make([]byte, 0, len(call.ID)+len(call.Name)+len(args)+32)
	out = append(out, `{"call_id":`...)
	out, _ = jsontext.AppendQuote(out, call.ID)
	out = append(out, `,"tool":`...)
	out, _ = jsontext.AppendQuote(out, call.Name)
	out = append(out, `,"args":`...)
	out = append(out, args...)
	return append(out, '}')
}

func parseDecision(value jsontext.Value) (HookDecision, error) {
	decoded, err := corescript.FromJSON(value)
	if err != nil || decoded == starlark.None {
		return HookDecision{}, err
	}
	dict, ok := decoded.(*starlark.Dict)
	if !ok {
		return HookDecision{}, errors.New(`hook result must be None or {"decision": "allow"|"deny", "reason": str}`)
	}
	decision, _ := dictString(dict, "decision")
	reason, hasReason := dictString(dict, "reason")
	switch {
	case decision == DecisionAllow:
		return HookDecision{Decision: DecisionAllow, Reason: reason}, nil
	case decision == DecisionDeny && hasReason:
		return HookDecision{Decision: DecisionDeny, Reason: reason}, nil
	default:
		return HookDecision{}, errors.New(`hook result must be None or {"decision": "allow"|"deny", "reason": str}`)
	}
}

func dictString(dict *starlark.Dict, key string) (string, bool) {
	value, found, _ := dict.Get(starlark.String(key))
	if !found {
		return "", false
	}
	return starlark.AsString(value)
}
