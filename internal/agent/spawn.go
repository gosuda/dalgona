package agent

import (
	"cmp"
	"context"
	"fmt"
	"net/url"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/task"
	"github.com/gosuda/dalgona/internal/tool/yield"
)

const (
	maxChildren       = 8
	yieldNudge        = "Call yield now: data on success, error on failure."
	noYieldPrefix     = "subagent ended without yield; last message: "
	lastMessageLength = 2000
)

type childStage int

const (
	stageRunning childStage = iota + 1
	stageNeedsPrompt
	stageNeedsNudge
	stageYielded
	stageGaveUp
)

type childReport struct {
	stage  childStage
	result tool.Result
	damage error
}

func (t *Thread) withSubagentTools(tools []tool.Tool) []tool.Tool {
	if t.created.Advisor != "" {
		return tools
	}
	if t.created.Parent != "" {
		return append(tools, yield.New())
	}
	if t.host.cfg.Product == config.ProductDalgona {
		return append(tools, task.New(t))
	}
	return tools
}

func (t *Thread) Spawn(ctx context.Context, callID string, req task.Request) (tool.Result, error) {
	select {
	case t.children <- struct{}{}:
	case <-ctx.Done():
		return tool.Result{}, context.Cause(ctx)
	}
	defer func() { <-t.children }()
	child, err := t.childFor(ctx, callID, req)
	if err != nil {
		return tool.Result{}, err
	}
	result, err := t.supervise(ctx, child, req.Prompt)
	if err != nil && ctx.Err() != nil {
		child.Interrupt()
	}
	return result, err
}

func (t *Thread) childFor(ctx context.Context, callID string, req task.Request) (*Thread, error) {
	raw, err := url.PathUnescape(callID)
	if err == nil {
		callID = raw
	}
	v, err := t.view()
	if err != nil {
		return nil, err
	}
	spec, err := t.childSpec(ctx, req, pathSettings(t.created, v.records))
	if err != nil {
		return nil, err
	}
	id, spawned := childOf(v.state, callID)
	if !spawned {
		id = t.host.newThreadID()
		if _, err = t.appendEvents(ctx, event.ChildSpawned{CallID: callID, ChildID: id, Runner: spec.Runner}); err != nil {
			return nil, err
		}
	}
	child, err := t.host.openChild(ctx, spec, id)
	t.notifyChildStarted(child, req, err)
	return child, err
}

func (t *Thread) notifyChildStarted(child *Thread, req task.Request, err error) {
	if err != nil || t.host.cfg.OnChildStarted == nil {
		return
	}
	if req.Advisor || childAdvisorConfigured(t.host.cfg.Config) {
		t.host.cfg.OnChildStarted(child, req)
	}
}

func childAdvisorConfigured(cfg config.Config) bool {
	if !cfg.Advisor.Enabled {
		return false
	}
	if cfg.Advisor.Subagents {
		return true
	}
	for _, spec := range cfg.Advisors {
		if spec.Subagents == config.AdvisorSubagentsOn {
			return true
		}
	}
	return false
}

func childOf(state event.State, callID string) (string, bool) {
	for id, child := range state.Children {
		if child.CallID == callID {
			return id, true
		}
	}
	return "", false
}

func (t *Thread) childSpec(ctx context.Context, req task.Request, current Settings) (ThreadSpec, error) {
	spec := ThreadSpec{
		Cwd: t.created.Cwd, Model: current.Model, Approval: current.Approval,
		Sandbox: current.Sandbox, Runner: cmp.Or(req.Runner, t.created.Runner), Parent: t.id,
	}
	if req.Model == "" {
		return spec, nil
	}
	resolved, err := t.host.ResolveRole(ctx, req.Model, current.Model)
	if err != nil {
		return spec, fmt.Errorf("agent: task model %q: %w", req.Model, err)
	}
	spec.Model = resolved
	return spec, nil
}

func (h *Host) openChild(ctx context.Context, spec ThreadSpec, id string) (*Thread, error) {
	h.mu.Lock()
	_, exists := h.sessions[id]
	h.mu.Unlock()
	if exists {
		return h.Resume(ctx, id)
	}
	return h.startSession(ctx, spec, id)
}

func (t *Thread) supervise(ctx context.Context, child *Thread, prompt string) (tool.Result, error) {
	for {
		report, err := ask(child, func(a *actor) childReport { return a.childReport() })
		if err != nil {
			return tool.Result{}, err
		}
		switch report.stage {
		case stageYielded, stageGaveUp:
			return report.result, report.damage
		case stageNeedsPrompt:
			err = child.Submit(ctx, Input{Kind: InputPrompt, Text: prompt})
		case stageNeedsNudge:
			err = child.Submit(ctx, Input{Kind: InputFollowup, Text: yieldNudge})
		case stageRunning:
			err = waitTurnEnd(ctx, child)
		}
		if err != nil {
			return tool.Result{}, err
		}
	}
}

func waitTurnEnd(ctx context.Context, child *Thread) error {
	updates := child.Subscribe()
	defer func() { child.Unsubscribe(updates) }()
	for {
		status, err := child.Status()
		if err != nil || !status.Busy {
			return err
		}
		select {
		case _, open := <-updates:
			if !open {
				updates = child.Subscribe()
			}
		case <-ctx.Done():
			return context.Cause(ctx)
		}
	}
}

func (a *actor) childReport() childReport {
	if a.damage != nil {
		return childReport{stage: stageGaveUp, result: tool.Result{Output: []byte(a.damage.Error())}, damage: a.damage}
	}
	if a.busy || a.state.TurnID != "" {
		return childReport{stage: stageRunning}
	}
	history := scanChild(a.records)
	switch {
	case history.prompts == 0:
		return childReport{stage: stageNeedsPrompt}
	case history.yield != nil:
		return childReport{stage: stageYielded, result: a.t.yieldResult(*history.yield)}
	case !history.nudged:
		return childReport{stage: stageNeedsNudge}
	default:
		text, err := a.t.host.loadText(history.last.Text, history.last.TextBlob)
		if err != nil {
			a.t.host.log.Warn("agent: load the last subagent message", "thread", a.t.id, "error", err)
		}
		return childReport{stage: stageGaveUp, result: tool.Result{Output: []byte(noYieldPrefix + previewRunes(text, lastMessageLength))}}
	}
}

type childHistory struct {
	prompts int
	nudged  bool
	yield   *event.ToolStarted
	last    event.AssistantMessage
	yields  map[string]event.ToolStarted
}

func scanChild(records []event.Seqd) childHistory {
	history := childHistory{yields: make(map[string]event.ToolStarted)}
	for _, item := range records {
		history.add(item.Event)
	}
	return history
}

func (h *childHistory) add(e event.Event) {
	switch value := e.(type) {
	case event.UserMessage:
		h.prompts++
		h.nudged = h.nudged || value.Text == yieldNudge
	case event.AssistantMessage:
		h.last = value
	case event.ToolStarted:
		if value.Tool == yield.Name {
			h.yields[value.CallID] = value
		}
	case event.ToolCompleted:
		if started, ok := h.yields[value.CallID]; ok && value.OK {
			h.yield = &started
		}
	}
}

func (t *Thread) yieldResult(started event.ToolStarted) tool.Result {
	args, err := t.host.loadRaw(started.Args, started.ArgsBlob)
	if err != nil {
		return tool.Result{Output: []byte(task.ErrorText(err.Error()))}
	}
	parsed, err := yield.Parse(args)
	switch {
	case err != nil:
		return tool.Result{Output: []byte(task.ErrorText(err.Error()))}
	case parsed.IsError:
		return tool.Result{Output: []byte(task.ErrorText(parsed.Error))}
	default:
		return tool.Result{Output: []byte(task.DataText(parsed.Data)), OK: true}
	}
}

func previewRunes(text string, limit int) string {
	runes := []rune(text)
	return string(runes[:min(len(runes), limit)])
}

func yieldedIn(v view, calls []event.ToolCall) bool {
	outputs := make(map[string]bool, len(calls))
	for _, item := range v.records {
		if done, ok := item.Event.(event.ToolCompleted); ok {
			outputs[done.CallID] = done.OK
		}
	}
	for _, call := range calls {
		if call.Name == yield.Name && outputs[call.ID] {
			return true
		}
	}
	return false
}
