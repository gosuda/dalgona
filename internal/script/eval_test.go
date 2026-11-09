package script_test

import (
	"context"
	"encoding/json/jsontext"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/script"
	"github.com/gosuda/dalgona/internal/tool"
)

func specs(names ...string) []tool.Spec {
	out := make([]tool.Spec, len(names))
	for i, name := range names {
		out[i] = tool.Spec{Name: name, Description: "Run " + name + ".\nSecond line."}
	}
	return out
}

var threadTools = specs("read", "search", "patch", "exec", "web_fetch", "task", "todo", "mcp__docs__lookup", "eval")

func names(list []tool.Spec) []string {
	out := make([]string, len(list))
	for i, spec := range list {
		out[i] = spec.Name
	}
	return out
}

func echoTool(_ context.Context, name string, args jsontext.Value) (jsontext.Value, error) {
	return jsontext.Value(`{"tool":"` + name + `","args":` + string(args) + `}`), nil
}

func TestEvalCellReturnsLastExpressionAndPrints(t *testing.T) {
	j := newJournal(echoTool)
	eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeEvalOnly, Tools: threadTools})
	got := runEval(t.Context(), t, eval, "call-1", `{"code":"print('a\\nb')\nx = data['n']\nx * 2","data":{"n":21}}`)
	if got.Status != "completed" || string(got.Value) != "42" || got.InvocationID != "call-1" {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
	if !slices.Equal(got.Prints.Lines, []string{"a", "b"}) || got.Prints.Truncated {
		t.Fatalf("prints = %+v", got.Prints)
	}
	if done := j.completed["call-1"]; done.Status != script.StatusCompleted || string(done.Value) != "42" {
		t.Fatalf("completed outcome = %+v", done)
	}
}

func TestEvalSyntaxErrorStopsAtParseStage(t *testing.T) {
	eval := script.NewEval(newJournal(echoTool), script.EvalOptions{Mode: config.ToolModeEvalOnly, Tools: threadTools})
	got := runEval(t.Context(), t, eval, "call-1", `{"code":"x = ("}`)
	if got.Status != "failed" || errorCode(got) != "syntax" || got.Error.Stage != "parse" {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
}

func TestEvalEmptyUsesStopsToolCallWithScopeExceeded(t *testing.T) {
	j := newJournal(echoTool)
	eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeEvalOnly, Tools: threadTools})
	got := runEval(t.Context(), t, eval, "call-1", `{"code":"r = ctx.try_call(tools.exec, command = 'ls')\nr","uses":[]}`)
	if errorCode(got) != "scope_exceeded" || got.Status != "denied" {
		t.Fatalf("reply = %+v, want scope_exceeded that try_call cannot catch", got)
	}
	if executed, _ := j.counts(); executed != 0 {
		t.Fatalf("executed %d tool calls under uses = []", executed)
	}
}

func TestEvalUsesOutsideAllowListIsRejectedBeforeRun(t *testing.T) {
	j := newJournal(echoTool)
	eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeNormal, Tools: threadTools, Config: config.EvalConfig{Uses: []string{"read"}, Set: true}})
	got := runEval(t.Context(), t, eval, "call-1", `{"code":"1","uses":["exec"]}`)
	if errorCode(got) != "scope_exceeded" || got.Error.Stage != "uses" {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
	if len(j.starts) != 0 {
		t.Fatal("a rejected cell started a durable run")
	}
}

func TestEvalCellKilledMidExecRecoversIndeterminate(t *testing.T) {
	j := newJournal(echoTool)
	eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeEvalOnly, Tools: threadTools})
	args := `{"code":"a = tools.read(path = 'x')\nb = tools.exec(command = 'make')\n[a, b]"}`
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	j.killDuring, j.kill = 2, cancel
	if got := runEval(ctx, t, eval, "call-1", args); got.Status != "cancelled" {
		t.Fatalf("killed cell status = %s", got.Status)
	}

	got := runEval(t.Context(), t, eval, "call-1", args)
	if got.Status != "indeterminate" || errorCode(got) != "indeterminate" {
		t.Fatalf("recovered reply = %+v", got)
	}
	if executed, _ := j.counts(); executed != 1 {
		t.Fatalf("recovery executed %d calls; the interrupted exec must not run again", executed)
	}
}

func TestEvalTryCallCatchesToolFailure(t *testing.T) {
	j := newJournal(func(context.Context, string, jsontext.Value) (jsontext.Value, error) { return nil, errToolFailed })
	eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeEvalOnly, Tools: threadTools})
	got := runEval(t.Context(), t, eval, "call-1", `{"code":"r = ctx.try_call(tools.exec, command = 'x')\n[r.ok, r.error]"}`)
	if string(got.Value) != `[false,"tool failed"]` {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
	uncaught := runEval(t.Context(), t, eval, "call-2", `{"code":"tools.exec(command = 'x')"}`)
	if uncaught.Status != "failed" || !strings.Contains(uncaught.Error.Message, "tool failed") {
		t.Fatalf("uncaught failure reply = %+v", uncaught)
	}
}

func TestToolModeLists(t *testing.T) {
	cases := []struct {
		mode    config.ToolMode
		visible []string
		folded  []string
	}{
		{config.ToolModeNormal, names(threadTools), nil},
		{config.ToolModeEvalFirst, []string{"read", "search", "patch", "exec", "eval"}, []string{"web_fetch", "task", "todo", "mcp__docs__lookup"}},
		{config.ToolModeEvalOnly, []string{"eval"}, []string{"read", "search", "patch", "exec", "web_fetch", "task", "todo", "mcp__docs__lookup"}},
	}
	for _, c := range cases {
		if got := names(script.ModelTools(c.mode, threadTools)); !slices.Equal(got, c.visible) {
			t.Errorf("%s visible = %v, want %v", c.mode, got, c.visible)
		}
		folded := names(script.FoldedTools(c.mode, threadTools))
		if !slices.Equal(slices.DeleteFunc(folded, func(n string) bool { return n == "eval" }), c.folded) {
			t.Errorf("%s folded = %v, want %v", c.mode, folded, c.folded)
		}
	}
}

func TestCheckDirectRejectsFoldedTools(t *testing.T) {
	err := script.CheckDirect(config.ToolModeEvalFirst, "web_fetch")
	if err == nil || err.Error() != "tool web_fetch is not available in tool_mode eval-first; call it from eval" {
		t.Fatalf("direct web_fetch error = %v", err)
	}
	if err := script.CheckDirect(config.ToolModeEvalFirst, "exec"); err != nil {
		t.Fatalf("direct exec error = %v", err)
	}
	if err := script.CheckDirect(config.ToolModeEvalOnly, "read"); err == nil {
		t.Fatal("eval-only allowed a direct read")
	}
	if err := script.CheckDirect(config.ToolModeNormal, "web_fetch"); err != nil {
		t.Fatalf("normal mode rejected a direct call: %v", err)
	}
}

func TestEvalFirstFoldsToolsIntoEval(t *testing.T) {
	j := newJournal(echoTool)
	eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeEvalFirst, Tools: threadTools})
	desc := eval.Spec().Description
	if !strings.Contains(desc, "\nweb_fetch: Run web_fetch.") || strings.Contains(desc, "\nexec: ") || strings.Contains(desc, "Second line") {
		t.Fatalf("eval description does not list exactly the folded tools:\n%s", desc)
	}
	got := runEval(t.Context(), t, eval, "call-1", `{"code":"tools.web_fetch(url = 'https://example.com')['tool']"}`)
	if string(got.Value) != `"web_fetch"` {
		t.Fatalf("folded call reply = %+v", got)
	}
	exec := runEval(t.Context(), t, eval, "call-2", `{"code":"tools.exec(command = 'ls')"}`)
	if errorCode(exec) != "scope_exceeded" {
		t.Fatalf("eval-first default allow list let a listed tool run in a cell: %+v", exec)
	}
}
