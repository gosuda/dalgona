package script_test

import (
	"encoding/json/jsontext"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/script"
	"github.com/gosuda/dalgona/internal/tool"
)

func writePlugin(t *testing.T, home, name, src string) {
	t.Helper()
	dir := filepath.Join(home, "plugins")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, name), []byte(src), 0o600); err != nil {
		t.Fatal(err)
	}
}

func TestPluginEffectsAtLoadTimeAreRejected(t *testing.T) {
	for _, effect := range []string{"time.time()", "time.sleep(0)"} {
		home := t.TempDir()
		writePlugin(t, home, "bad.star", "started = "+effect+"\n")
		writePlugin(t, home, "good.star", "def impl(ctx):\n    return 1\ncommand('hi', 'Say hi.', impl)\n")
		plugins, err := script.LoadPlugins(home)
		if err == nil || !strings.Contains(err.Error(), "plugin bad.star: effects are not allowed at load time") {
			t.Fatalf("%s: load error = %v", effect, err)
		}
		if commands := plugins.Commands(); len(commands) != 1 || commands[0].Name != "hi" {
			t.Fatalf("%s: a bad file blocked the good one: %v", effect, commands)
		}
	}
}

const qualityPlugin = `
def todos(ctx, path = "."):
    found = ctx.tools.search(pattern = "TODO", path = path)
    return {"found": found["args"], "at": time.time() > 0}

tool("quality.todos", "Count TODO markers.\nLonger help.", {"type": "object", "properties": {"path": {"type": "string"}}}, todos)

def guard(ctx, event):
    if event["tool"] == "exec" and "rm -rf" in event["args"]["command"]:
        return {"decision": "deny", "reason": "destructive"}
    if event["tool"] == "read":
        return {"decision": "allow"}
    return None

on("tool_call", guard)

def greet(ctx, args):
    ctx.log("greeted " + args)
    return "hello " + args

command("greet", "Greet someone.", greet)
`

func loadPlugins(t *testing.T, files map[string]string) *script.Plugins {
	t.Helper()
	home := t.TempDir()
	for name, src := range files {
		writePlugin(t, home, name, src)
	}
	plugins, err := script.LoadPlugins(home)
	if err != nil {
		t.Fatal(err)
	}
	return plugins
}

func TestPluginToolRunsAsDurableScript(t *testing.T) {
	j := newJournal(echoTool)
	tools := loadPlugins(t, map[string]string{"quality.star": qualityPlugin}).Tools(j)
	if len(tools) != 1 || tools[0].Spec().Name != "quality.todos" || tools[0].Class() != tool.ClassScript {
		t.Fatalf("plugin tools = %v", tools)
	}
	result, err := tools[0].Run(t.Context(), tool.Call{ID: "call-9", Name: "quality.todos", Args: jsontext.Value(`{"path":"src"}`)}, tool.Env{})
	if err != nil {
		t.Fatal(err)
	}
	got := decodeReply(t, result)
	if got.Status != "completed" || string(got.Value) != `{"found":{"pattern":"TODO","path":"src"},"at":true}` {
		t.Fatalf("reply = %+v", got)
	}
	requirePluginRunJournaled(t, j, "call-9")
}

func requirePluginRunJournaled(t *testing.T, j *journal, id string) {
	t.Helper()
	if rec, ok := j.Lookup(id, 1); !ok || rec.Op != script.ToolOp("search") {
		t.Fatalf("inner call not journaled under the plugin run: %+v", rec)
	}
	if start := j.starts[id]; start.Entry != "plugins/quality.star" || string(start.Source) != qualityPlugin {
		t.Fatalf("run start = %+v", start)
	}
}

func TestToolCallHookDecisions(t *testing.T) {
	plugins := loadPlugins(t, map[string]string{"quality.star": qualityPlugin})
	j := newJournal(echoTool)
	cases := []struct {
		call tool.Call
		want script.HookDecision
	}{
		{tool.Call{ID: "c1", Name: "exec", Args: jsontext.Value(`{"command":"rm -rf /"}`)}, script.HookDecision{Decision: "deny", Reason: "destructive", By: "hook:quality.star:guard"}},
		{tool.Call{ID: "c2", Name: "read", Args: jsontext.Value(`{"path":"a"}`)}, script.HookDecision{Decision: "allow", By: "hook:quality.star:guard"}},
		{tool.Call{ID: "c3", Name: "exec", Args: jsontext.Value(`{"command":"ls"}`)}, script.HookDecision{}},
	}
	for _, c := range cases {
		got, err := plugins.ToolCall(t.Context(), j, c.call.ID, c.call)
		if err != nil || got != c.want {
			t.Fatalf("%s: decision = %+v, %v; want %+v", c.call.ID, got, err, c.want)
		}
		if done, ok := j.completed[c.call.ID+"/hook:quality.star:guard"]; !ok || done.Status != script.StatusCompleted {
			t.Fatalf("%s: hook run completion = %+v, %v", c.call.ID, done, ok)
		}
	}
}

func TestPluginCommandAndInvalidHookResult(t *testing.T) {
	plugins := loadPlugins(t, map[string]string{
		"quality.star": qualityPlugin,
		"zz.star":      "def bad(ctx, event):\n    return {\"decision\": \"deny\"}\non('tool_call', bad)\n",
	})
	j := newJournal(echoTool)
	outcome, err := plugins.RunCommand(t.Context(), j, "cmd-1", "greet", "ana")
	if err != nil || string(outcome.Value) != `"hello ana"` || len(j.logs) != 1 {
		t.Fatalf("command outcome = %+v, %v, logs %v", outcome, err, j.logs)
	}
	_, err = plugins.ToolCall(t.Context(), j, "c4", tool.Call{ID: "c4", Name: "exec", Args: jsontext.Value(`{"command":"ls"}`)})
	if err == nil || !strings.Contains(err.Error(), "hook:zz.star:bad") {
		t.Fatalf("deny without reason error = %v", err)
	}
}
