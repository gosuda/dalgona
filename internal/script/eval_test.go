package script_test

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
	readtool "github.com/gosuda/dalgona/internal/dal/tool/read"
	rewritetool "github.com/gosuda/dalgona/internal/dal/tool/rewrite"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
	"github.com/gosuda/dalgona/internal/script"
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

func decodeString(t *testing.T, v jsontext.Value) string {
	t.Helper()
	var s string
	if err := json.Unmarshal([]byte(v), &s); err != nil {
		t.Fatalf("reply value is not a JSON string: %v", err)
	}
	return s
}

func echoTool(_ context.Context, name string, args jsontext.Value) (jsontext.Value, error) {
	return jsontext.Value(`{"tool":"` + name + `","args":` + string(args) + `}`), nil
}

func readFixtureTool(t *testing.T, name, content string) toolFunc {
	t.Helper()
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, name), []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	local, err := runner.NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = local.Close() })
	env := tool.Env{Runner: local, Cwd: dir, Home: t.TempDir(), Snapshots: snapshot.New()}
	return func(ctx context.Context, toolName string, args jsontext.Value) (jsontext.Value, error) {
		if toolName != "read" {
			return echoTool(ctx, toolName, args)
		}
		result, err := readtool.Tool{}.Run(ctx, tool.Call{ID: "read", Name: "read", Args: args}, env)
		if err != nil {
			return nil, err
		}
		return jsontext.AppendQuote(nil, string(result.Output))
	}
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

func TestEvalFirstListsRatchet(t *testing.T) {
	all := specs("read", "exec", "ratchet", "eval")
	if got, want := names(script.ModelTools(config.ToolModeEvalFirst, all)), []string{"read", "exec", "ratchet", "eval"}; !slices.Equal(got, want) {
		t.Fatalf("eval-first visible = %v, want %v", got, want)
	}
}

func TestEvalOnlyListsRatchet(t *testing.T) {
	all := specs("read", "ratchet", "ask", "eval")
	if got, want := names(script.ModelTools(config.ToolModeEvalOnly, all)), []string{"ask", "ratchet", "eval"}; !slices.Equal(got, want) {
		t.Fatalf("eval-only visible = %v, want %v", got, want)
	}
	if err := script.CheckDirect(config.ToolModeEvalOnly, "ratchet"); err != nil {
		t.Fatalf("eval-only rejected a direct ratchet call: %v", err)
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
	j := newJournal(readFixtureTool(t, "a.txt", "folded reach bytes"))
	eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeEvalFirst, Tools: threadTools})
	desc := eval.Spec().Description
	if !strings.Contains(desc, "\nweb_fetch: Run web_fetch.") || strings.Contains(desc, "\nexec: ") || strings.Contains(desc, "Second line") {
		t.Fatalf("eval description does not list exactly the folded tools:\n%s", desc)
	}
	got := runEval(t.Context(), t, eval, "call-1", `{"code":"tools.web_fetch(url = 'https://example.com')['tool']"}`)
	if string(got.Value) != `"web_fetch"` {
		t.Fatalf("folded call reply = %+v", got)
	}
	read := runEval(t.Context(), t, eval, "call-2", `{"code":"tools.read(path = 'a.txt')"}`)
	if read.Status != "completed" || !strings.Contains(decodeString(t, read.Value), "1:folded reach bytes\n") {
		t.Fatalf("eval-first cell did not read the fixture through the real read tool: %+v error = %+v", read, read.Error)
	}
}

func TestAskStaysDirectInEveryToolMode(t *testing.T) {
	all := specs("read", "exec", "ask", "web_fetch", "eval")
	cases := []struct {
		mode    config.ToolMode
		visible []string
	}{
		{config.ToolModeEvalFirst, []string{"read", "exec", "ask", "eval"}},
		{config.ToolModeEvalOnly, []string{"ask", "eval"}},
	}
	for _, c := range cases {
		if got := names(script.ModelTools(c.mode, all)); !slices.Equal(got, c.visible) {
			t.Errorf("%s visible = %v, want %v", c.mode, got, c.visible)
		}
		if slices.Contains(names(script.FoldedTools(c.mode, all)), "ask") {
			t.Errorf("%s folded the direct ask tool", c.mode)
		}
	}
}

func TestScriptCannotCallAsk(t *testing.T) {
	j := newJournal(echoTool)
	eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeNormal, Tools: specs("ask", "eval"), Config: config.EvalConfig{Uses: []string{"ask"}, Set: true}})
	got := runEval(t.Context(), t, eval, "call-1", `{"code":"tools.ask(questions = [])"}`)
	if got.Status != "denied" || errorCode(got) != "scope_exceeded" || got.Error.Message != "tool ask needs the user and runs only as a direct call" {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
}

func TestNormalCellReachesEveryToolWithoutUses(t *testing.T) {
	j := newJournal(readFixtureTool(t, "a.txt", "plain cell bytes"))
	eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeNormal, Tools: threadTools})
	got := runEval(t.Context(), t, eval, "call-1", `{"code":"tools.read(path = 'a.txt')"}`)
	if got.Status != "completed" || !strings.Contains(decodeString(t, got.Value), "1:plain cell bytes\n") {
		t.Fatalf("normal cell reply = %+v error = %+v", got, got.Error)
	}
	if executed, _ := j.counts(); executed != 1 {
		t.Fatalf("executed %d tool calls, want 1", executed)
	}
}

func TestCellsDoNotReachYield(t *testing.T) {
	j := newJournal(echoTool)
	eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeNormal, Tools: specs("read", "yield", "eval")})
	got := runEval(t.Context(), t, eval, "call-1", `{"code":"tools['yield'](data = 1)"}`)
	if got.Status != "failed" || errorCode(got) != "error" || !strings.Contains(got.Error.Message, "tool yield is not reachable from this script") {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
	if executed, _ := j.counts(); executed != 0 {
		t.Fatalf("yield refusal still executed %d tool calls", executed)
	}
	listed := runEval(t.Context(), t, eval, "call-2", `{"code":"'yield' in dir(tools) and 'read' in dir(tools)"}`)
	if string(listed.Value) != "false" {
		t.Fatalf("tools reach lists yield or hides read: %+v error = %+v", listed, listed.Error)
	}
}

func TestCellsRefuseUserCallsAsDirectOnly(t *testing.T) {
	tools := specs("ask", "ratchet", "read", "eval")
	refused := []struct{ name, code, message string }{
		{"ask", `tools.ask(questions = [])`, "tool ask needs the user and runs only as a direct call"},
		{"ratchet approve", `tools.ratchet(action = 'approve', flow = 'x')`, "tool ratchet approve needs the user and runs only as a direct call"},
	}
	for _, c := range refused {
		j := newJournal(echoTool)
		eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeNormal, Tools: tools})
		got := runEval(t.Context(), t, eval, "call-1", `{"code":"`+c.code+`"}`)
		if got.Status != "denied" || errorCode(got) != "scope_exceeded" || got.Error.Message != c.message {
			t.Fatalf("%s: reply = %+v error = %+v", c.name, got, got.Error)
		}
		if executed, _ := j.counts(); executed != 0 {
			t.Fatalf("%s: refusal still executed %d tool calls", c.name, executed)
		}
	}
	eval := script.NewEval(newJournal(echoTool), script.EvalOptions{Mode: config.ToolModeNormal, Tools: tools})
	status := runEval(t.Context(), t, eval, "call-2", `{"code":"tools.ratchet(action = 'status', flow = 'x')['tool']"}`)
	if string(status.Value) != `"ratchet"` {
		t.Fatalf("ratchet status reply = %+v error = %+v", status, status.Error)
	}
}

func TestYieldStaysDirect(t *testing.T) {
	cases := []struct {
		mode    config.ToolMode
		all     []tool.Spec
		visible []string
	}{
		{config.ToolModeEvalFirst, specs("read", "search", "patch", "exec", "todo", "web_fetch", "yield", "eval"), []string{"read", "search", "patch", "exec", "yield", "eval"}},
		{config.ToolModeEvalOnly, specs("read", "yield", "eval"), []string{"yield", "eval"}},
	}
	for _, c := range cases {
		if got := names(script.ModelTools(c.mode, c.all)); !slices.Equal(got, c.visible) {
			t.Errorf("%s visible = %v, want %v", c.mode, got, c.visible)
		}
		if err := script.CheckDirect(c.mode, "yield"); err != nil {
			t.Errorf("%s refused a direct yield: %v", c.mode, err)
		}
	}
}

func TestCellCallCapStopsRunawayLoop(t *testing.T) {
	j := newJournal(readFixtureTool(t, "x.txt", "cap bytes"))
	eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeNormal, Tools: threadTools})
	got := runEval(t.Context(), t, eval, "call-1", `{"code":"for i in range(1025):\n    tools.read(path = 'x.txt')"}`)
	if got.Status != "limit_exceeded" || errorCode(got) != "limit_exceeded" {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
	if executed, _ := j.counts(); executed != 1024 {
		t.Fatalf("executed %d tool calls, want 1024", executed)
	}
	within := runEval(t.Context(), t, eval, "call-2", `{"code":"for i in range(1024):\n    tools.read(path = 'x.txt')"}`)
	if within.Status != "completed" {
		t.Fatalf("1024-call cell reply = %+v error = %+v", within, within.Error)
	}
}

func TestCellUsesExcludingReadDeniesWithoutReadEffect(t *testing.T) {
	j := newJournal(readFixtureTool(t, "a.txt", "must not be read"))
	eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeNormal, Tools: threadTools, Config: config.EvalConfig{Uses: []string{"exec"}, Set: true}})
	got := runEval(t.Context(), t, eval, "call-1", `{"code":"tools.read(path = 'a.txt')"}`)
	if got.Status != "denied" || errorCode(got) != "scope_exceeded" || got.Error.Message != "tool read is not in this script's uses" {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
	if executed, _ := j.counts(); executed != 0 {
		t.Fatalf("denied read still executed %d tool calls", executed)
	}
	allowed := runEval(t.Context(), t, eval, "call-2", `{"code":"tools.exec(command = 'ls')['tool']"}`)
	if string(allowed.Value) != `"exec"` {
		t.Fatalf("uses-listed exec reply = %+v error = %+v", allowed, allowed.Error)
	}
}

const (
	rewriteCellCode = `tools.rewrite(lang = 'go', pattern = '((identifier) @name (#eq? @name "Old"))', target = 'name', replace = 'New', path = 'a.go')`
	rewriteSelected = "package a\n\nfunc Old() {}\n"
	rewriteOther    = "package b\n\nfunc Old() {}\n"
)

func rewriteFixtureTool(t *testing.T) (toolFunc, string) {
	t.Helper()
	dir := t.TempDir()
	for name, body := range map[string]string{"a.go": rewriteSelected, "b.go": rewriteOther} {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	local, err := runner.NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = local.Close() })
	env := tool.Env{Runner: local, Cwd: dir, Home: t.TempDir(), Snapshots: snapshot.New()}
	editor := rewritetool.New()
	return func(ctx context.Context, toolName string, args jsontext.Value) (jsontext.Value, error) {
		if toolName != "rewrite" {
			return echoTool(ctx, toolName, args)
		}
		result, err := editor.Run(ctx, tool.Call{ID: "rewrite", Name: "rewrite", Args: args}, env)
		if err != nil {
			return nil, err
		}
		return jsontext.AppendQuote(nil, string(result.Output))
	}, dir
}

func rewriteCellArgs(t *testing.T) string {
	t.Helper()
	raw, err := json.Marshal(map[string]string{"code": rewriteCellCode})
	if err != nil {
		t.Fatal(err)
	}
	return string(raw)
}

func requireFileBytes(t *testing.T, dir, name, want string) {
	t.Helper()
	got, err := os.ReadFile(filepath.Join(dir, name))
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != want {
		t.Fatalf("%s = %q, want %q", name, got, want)
	}
}

func TestRewriteIsReachableFromCells(t *testing.T) {
	fx, dir := rewriteFixtureTool(t)
	j := newJournal(fx)
	eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeEvalFirst, Tools: specs("rewrite", "exec", "eval")})
	got := runEval(t.Context(), t, eval, "call-1", rewriteCellArgs(t))
	if got.Status != "completed" || !strings.Contains(decodeString(t, got.Value), "a.go") {
		t.Fatalf("cell call to rewrite = %+v error = %+v", got, got.Error)
	}
	if executed, _ := j.counts(); executed != 1 {
		t.Fatalf("executed %d tool calls, want 1", executed)
	}
	requireFileBytes(t, dir, "a.go", "package a\n\nfunc New() {}\n")
	requireFileBytes(t, dir, "b.go", rewriteOther)
}

func TestCellUsesExcludingRewriteDeniesWithoutWriteEffect(t *testing.T) {
	fx, dir := rewriteFixtureTool(t)
	j := newJournal(fx)
	eval := script.NewEval(j, script.EvalOptions{Mode: config.ToolModeEvalFirst, Tools: specs("rewrite", "exec", "eval"), Config: config.EvalConfig{Uses: []string{"exec"}, Set: true}})
	got := runEval(t.Context(), t, eval, "call-1", rewriteCellArgs(t))
	if got.Status != "denied" || errorCode(got) != "scope_exceeded" || got.Error.Message != "tool rewrite is not in this script's uses" {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
	if executed, _ := j.counts(); executed != 0 {
		t.Fatalf("denied rewrite still executed %d tool calls", executed)
	}
	requireFileBytes(t, dir, "a.go", rewriteSelected)
	requireFileBytes(t, dir, "b.go", rewriteOther)
}
