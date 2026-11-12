package script

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"

	"go.starlark.net/starlark"

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
	files []*pluginFile
}

type Command struct {
	Name        string
	Description string
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
	call  func(r *run, impl starlark.Callable) (starlark.Value, error)
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

func (p *Plugins) loadFile(path string) error {
	src, err := os.ReadFile(path)
	if err != nil {
		return fmt.Errorf("read: %w", err)
	}
	name := filepath.Base(path)
	r := newRun(nil, "load:"+name, runConfig{})
	env, end := r.begin(context.Background(), 0)
	defer end()
	regs, err := r.load(name, src, env)
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

func (r *run) load(name string, src []byte, env starlark.StringDict) ([]registration, error) {
	var regs []registration
	add := func(reg registration) { regs = append(regs, reg) }
	env["tool"] = starlark.NewBuiltin("tool", registerTool(add))
	env["command"] = starlark.NewBuiltin("command", registerCommand(add))
	env["on"] = starlark.NewBuiltin("on", registerHook(add))
	r.loading = true
	_, err := r.execCell(name, src, env)
	r.loading = false
	if stop := r.fatalStop(); stop != nil {
		return nil, stop
	}
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
		schema, err := toJSON(params)
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

func (p *Plugins) invoke(ctx context.Context, fx Effects, inv invocation) (Outcome, error) {
	start, done, err := fx.Started(RunStart{ID: inv.runID, Entry: "plugins/" + inv.file.name, Source: inv.file.source, Args: inv.args, Seed: newSeed()})
	if err != nil {
		return Outcome{}, fmt.Errorf("plugin run %s: start run: %w", inv.runID, err)
	}
	if done != nil {
		return *done, nil
	}
	return complete(fx, start.ID, inv.run(ctx, fx, start))
}

func (inv invocation) run(ctx context.Context, fx Effects, start RunStart) Outcome {
	r := newRun(fx, start.ID, runConfig{})
	env, end := r.begin(ctx, start.Seed)
	defer end()
	regs, err := r.load(inv.file.name, start.Source, env)
	if err != nil {
		return r.finish(ctx, nil, err)
	}
	i := slices.IndexFunc(regs, func(reg registration) bool {
		return reg.kind == inv.reg.kind && reg.name == inv.reg.name && reg.fn == inv.reg.fn
	})
	if i < 0 {
		return stopped(StatusFailed, codeError, fmt.Sprintf("plugin %s no longer registers %s %s", inv.file.name, inv.reg.kind, inv.reg.name), stageRun)
	}
	value, err := inv.call(r, regs[i].impl)
	return r.finish(ctx, value, err)
}

func (p *Plugins) Tools(fx Effects) []tool.Tool {
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
	fx      Effects
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
		return replyResult(call.ID, stopped(StatusFailed, codeInvalid, fmt.Sprintf("%s: %v", t.reg.name, err), stageArgs))
	}
	outcome, err := t.plugins.invoke(ctx, t.fx, invocation{runID: call.ID, file: t.file, reg: t.reg, args: call.Args, call: func(r *run, impl starlark.Callable) (starlark.Value, error) {
		return starlark.Call(r.thread, impl, starlark.Tuple{r.ctxValue()}, kwargs)
	}})
	if err != nil {
		return tool.Result{}, err
	}
	return replyResult(call.ID, outcome)
}

func objectKwargs(args jsontext.Value) ([]starlark.Tuple, error) {
	decoded, err := fromJSON(args)
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

func (p *Plugins) RunCommand(ctx context.Context, fx Effects, runID, name, args string) (Outcome, error) {
	file, reg, ok := p.find(kindCommand, name)
	if !ok {
		return Outcome{}, fmt.Errorf("unknown command %s", name)
	}
	return p.invoke(ctx, fx, invocation{runID: runID, file: file, reg: reg, args: stringJSON(args), call: func(r *run, impl starlark.Callable) (starlark.Value, error) {
		return starlark.Call(r.thread, impl, starlark.Tuple{r.ctxValue(), starlark.String(args)}, nil)
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

func (p *Plugins) hook(ctx context.Context, fx Effects, runID string, target hookTarget, payload jsontext.Value) (Outcome, error) {
	event, err := fromJSON(payload)
	if err != nil {
		return Outcome{}, fmt.Errorf("%s: payload: %w", target.by(), err)
	}
	return p.invoke(ctx, fx, invocation{runID: runID + "/" + target.by(), file: target.file, reg: target.reg, args: payload, call: func(r *run, impl starlark.Callable) (starlark.Value, error) {
		return starlark.Call(r.thread, impl, starlark.Tuple{r.ctxValue(), event}, nil)
	}})
}

func (h hookTarget) failure(outcome Outcome, err error) error {
	if err != nil {
		return fmt.Errorf("%s: %w", h.by(), err)
	}
	if outcome.Status != StatusCompleted {
		return fmt.Errorf("%s: %s: %s", h.by(), outcome.Status, outcome.Error.Message)
	}
	return nil
}

func (h hookTarget) decision(outcome Outcome, err error) (HookDecision, error) {
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

func (p *Plugins) Emit(ctx context.Context, fx Effects, runID, event string, payload jsontext.Value) error {
	if event != EventSessionStart && event != EventTurnEnd {
		return fmt.Errorf("emit: event must be %q or %q, got %q", EventSessionStart, EventTurnEnd, event)
	}
	var errs []error
	for _, target := range p.hooks(event) {
		errs = append(errs, target.failure(p.hook(ctx, fx, runID, target, payload)))
	}
	return errors.Join(errs...)
}

func (p *Plugins) ToolCall(ctx context.Context, fx Effects, runID string, call tool.Call) (HookDecision, error) {
	payload := toolCallPayload(call)
	var allow, deny HookDecision
	var errs []error
	for _, target := range p.hooks(EventToolCall) {
		decision, err := target.decision(p.hook(ctx, fx, runID, target, payload))
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
	decoded, err := fromJSON(value)
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
