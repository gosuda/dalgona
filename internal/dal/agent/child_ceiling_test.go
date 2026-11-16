package agent

import (
	"context"
	"encoding/json/jsontext"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/tool"
	readtool "github.com/gosuda/dalgona/internal/dal/tool/read"
)

type stubTool struct {
	name  string
	class tool.Class
}

func (s stubTool) Spec() tool.Spec {
	return tool.Spec{Name: s.name, Description: "stub " + s.name, Schema: jsontext.Value(`{"type":"object","additionalProperties":false}`)}
}

func (s stubTool) Class() tool.Class { return s.class }

func (s stubTool) Run(context.Context, tool.Call, tool.Env) (tool.Result, error) {
	return tool.Result{Output: []byte(s.name + " ran"), OK: true}, nil
}

type pluginExtension struct {
	baseExtension
	t *Thread
}

func (e pluginExtension) Tools(builtin []tool.Tool) []tool.Tool {
	if e.t.created.Parent == "" {
		return builtin
	}
	return append(builtin, stubTool{name: "plugin_exec", class: tool.ClassExec})
}

func withToolsByModel(parentTools, childTools []tool.Tool) func(*harness) {
	return func(h *harness) {
		h.cfg.Tools = func(_ context.Context, modelID, _ string) ([]tool.Tool, error) {
			if modelID == childModel {
				return slices.Clone(childTools), nil
			}
			return slices.Clone(parentTools), nil
		}
		h.cfg.Extension = func(t *Thread) Extension { return pluginExtension{t: t} }
	}
}

const childModel = "openai/gpt-child"

func spawnedChildren(records []event.Seqd) []event.ChildSpawned {
	var out []event.ChildSpawned
	for _, item := range records {
		if spawned, ok := item.Event.(event.ChildSpawned); ok {
			out = append(out, spawned)
		}
	}
	return out
}

func sortedCopy(names []string) []string {
	return slices.Sorted(slices.Values(names))
}

func requestsWithPrompt(h *harness, prompt string) []map[string]any {
	var out []map[string]any
	for i := range h.model.count() {
		request := h.model.request(i)
		if slices.Contains(userTexts(request), prompt) {
			out = append(out, request)
		}
	}
	return out
}

type ceilingRun struct {
	recorded []string
	offered  []string
	outputs  map[string]string
	model    string
}

func runUnderCeiling(t *testing.T, parentTools, childTools []tool.Tool, first response) ceilingRun {
	t.Helper()
	h := newHarness(t, withToolsByModel(parentTools, childTools))
	h.write("notes.txt", "ceiling bytes")
	script := newChildScript(map[string][]response{"child a": {first, yieldData("ya", `"A"`)}})
	h.model.route = script.route
	h.model.push(calls(call{id: "ta", name: "task", args: `{"prompt":"child a","model":"` + childModel + `"}`}), text("parent done"))
	parent := h.idleThread("spawn")
	spawned := spawnedChildren(h.events(parent.ID()))
	if len(spawned) != 1 {
		t.Fatalf("child.spawned records = %d, want 1", len(spawned))
	}
	requests := requestsWithPrompt(h, "child a")
	if len(requests) < 2 {
		t.Fatalf("child model requests = %d, want at least 2", len(requests))
	}
	created, ok := h.events(spawned[0].ChildID)[0].Event.(event.SessionCreated)
	if !ok {
		t.Fatalf("first child record = %T, want session.created", h.events(spawned[0].ChildID)[0].Event)
	}
	return ceilingRun{
		recorded: spawned[0].Tools, offered: requestToolNames(requests[0]),
		outputs: callOutputs(requests[len(requests)-1]), model: created.Model,
	}
}

func TestParentToolCeiling(t *testing.T) {
	t.Parallel()
	readSearch := []tool.Tool{readtool.New(), stubTool{name: "search", class: tool.ClassRead}}
	wider := append(slices.Clone(readSearch), stubTool{name: "exec", class: tool.ClassExec}, stubTool{name: "patch", class: tool.ClassEdit})
	probe := calls(
		call{id: "cx", name: "exec", args: `{}`},
		call{id: "cp", name: "plugin_exec", args: `{}`},
		call{id: "cr", name: "read", args: `{"path":"notes.txt"}`},
	)
	got := runUnderCeiling(t, readSearch, wider, probe)
	if got.model != childModel {
		t.Fatalf("child model = %q, want %q", got.model, childModel)
	}
	want := []string{"read", "search", "yield"}
	if !slices.Equal(got.recorded, want) {
		t.Fatalf("child.spawned tools = %v, want %v", got.recorded, want)
	}
	if offered := sortedCopy(got.offered); !slices.Equal(offered, want) {
		t.Fatalf("child model tool list = %v, want %v", offered, want)
	}
	for _, id := range []string{"cx", "cp"} {
		if out := got.outputs[id]; !strings.Contains(out, "not found") {
			t.Fatalf("call %s outside the ceiling returned %q, want a not found refusal", id, out)
		}
	}
	if out := got.outputs["cr"]; !strings.Contains(out, "ceiling bytes") {
		t.Fatalf("call inside the ceiling did not read independently written bytes: %q", out)
	}
}

func TestParentToolCeilingPassesEveryToolButAskAndTask(t *testing.T) {
	t.Parallel()
	full := []tool.Tool{
		stubTool{name: "read", class: tool.ClassRead},
		stubTool{name: "search", class: tool.ClassRead},
		stubTool{name: "exec", class: tool.ClassExec},
		stubTool{name: "patch", class: tool.ClassEdit},
	}
	got := runUnderCeiling(t, full, full, calls(call{id: "cr", name: "read", args: `{}`}))
	want := []string{"exec", "patch", "read", "search", "yield"}
	if !slices.Equal(got.recorded, want) {
		t.Fatalf("child.spawned tools = %v, want %v", got.recorded, want)
	}
	if offered := sortedCopy(got.offered); !slices.Equal(offered, want) {
		t.Fatalf("child model tool list = %v, want %v", offered, want)
	}
	for _, withheld := range []string{"ask", "task", "plugin_exec"} {
		if slices.Contains(got.offered, withheld) {
			t.Fatalf("child model tool list %v offers %s", got.offered, withheld)
		}
	}
}

func TestChildToolCeilingRecovery(t *testing.T) {
	t.Parallel()
	crashed := newHarness(t)
	hold := make(chan struct{})
	t.Cleanup(func() { close(hold) })
	crashed.model.route = newChildScript(map[string][]response{"child a": {{body: sseText("never"), gate: hold}}}).route
	crashed.model.push(calls(call{id: "ta", name: "task", args: `{"prompt":"child a"}`}))
	parent := crashed.start()
	crashed.submit(parent, "spawn")
	crashed.waitRequests(2)
	spawned := spawnedChildren(crashed.events(parent.ID()))
	if len(spawned) != 1 || !slices.Equal(spawned[0].Tools, []string{"exec", "patch", "read", "search", "yield"}) {
		t.Fatalf("child.spawned before the crash = %+v", spawned)
	}
	home := t.TempDir()
	copyHome(t, crashed.home, home)

	late := stubTool{name: "late_tool", class: tool.ClassRead}
	revived := newHarness(t, func(h *harness) {
		h.cwd, h.cfg.Config.Cwd = crashed.cwd, crashed.cwd
		h.extraTools = []tool.Tool{late}
	})
	revived.model.route = newChildScript(map[string][]response{"child a": {yieldData("ya", `"A"`)}}).route
	revived.model.push(text("parent resumed"))
	host := revived.reopen(home)
	if err := host.Recover(t.Context()); err != nil {
		t.Fatal(err)
	}
	resumed, err := host.Resume(t.Context(), parent.ID())
	if err != nil {
		t.Fatal(err)
	}
	revived.waitIdle(resumed)
	childRequests := requestsWithPrompt(revived, "child a")
	if len(childRequests) == 0 {
		t.Fatal("the recovered child made no model request")
	}
	if offered := sortedCopy(requestToolNames(childRequests[0])); !slices.Equal(offered, spawned[0].Tools) {
		t.Fatalf("recovered child tool list = %v, want the child.spawned tools %v", offered, spawned[0].Tools)
	}
	parentRequests := requestsWithPrompt(revived, "spawn")
	if len(parentRequests) == 0 || !slices.Contains(requestToolNames(parentRequests[0]), "late_tool") {
		t.Fatal("the revived parent does not offer late_tool, so the recovery check proves nothing")
	}
}

func TestLegacyChildCeilingFailsClosed(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	parent := h.start()
	seed := []event.Event{
		event.TurnStarted{TurnID: "turn-seed"},
		event.ToolStarted{CallID: "legacy-call", TurnID: "turn-seed", Tool: "task", Args: jsontext.Value(`{}`), Class: "agent"},
		event.ChildSpawned{CallID: "legacy-call", ChildID: "legacy-child", Runner: "local"},
	}
	if err := parent.Append(t.Context(), seed...); err != nil {
		t.Fatal(err)
	}
	child, err := h.host.startSession(t.Context(), ThreadSpec{Parent: parent.ID()}, "legacy-child")
	if err != nil {
		t.Fatal(err)
	}
	before := h.model.count()
	err = child.Submit(t.Context(), Input{Kind: InputPrompt, Text: "legacy prompt"})
	if err == nil || !strings.Contains(err.Error(), "agent: child tool ceiling unavailable") {
		t.Fatalf("Submit without durable ceiling = %v", err)
	}
	if h.model.count() != before {
		t.Fatal("a child without a durable ceiling reached the model")
	}
}
