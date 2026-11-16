package scripthost_test

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
)

const policyPlugin = "def deny(ctx, event):\n    if event['tool'] == 'exec':\n        return {'decision': 'deny', 'reason': 'hooks forbid exec'}\n    return None\non('tool_call', deny)\n"

func withPolicy(t *testing.T) func(*harness) {
	return func(h *harness) { h.scripts.Plugins = loadPlugins(t, map[string]string{"policy.star": policyPlugin}) }
}

func TestPluginHookDeclinesThroughTheGate(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withPolicy(t), func(h *harness) { h.scripts.Core.Mode = config.ToolModeNormal })
	h.model.push(toolCall("x", "exec", `{"command":"echo hi"}`), text("done"))
	events := h.idle(h.run("run it"))
	resolved := decisions(events)["x"]
	if resolved.By != "hook:policy.star:deny" || resolved.Reason != "hooks forbid exec" || count(events, event.TypeToolStarted) != 0 {
		t.Fatalf("resolved = %+v", resolved)
	}
	if count(events, event.TypeScriptStarted) != 1 || count(events, event.TypeScriptCompleted) != 1 {
		t.Fatalf("hook runs: %d started, %d completed", count(events, event.TypeScriptStarted), count(events, event.TypeScriptCompleted))
	}
}

func TestScriptToolDenialStopsTheCell(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withPolicy(t))
	h.model.push(evalCall("tools.exec(command = \"echo hi\")"), text("done"))
	done := outputs(h.idle(h.run("run the cell")))
	if got := done["e"].Output; !strings.Contains(got, `"status":"denied"`) || !strings.Contains(got, "hooks forbid exec") {
		t.Fatalf("eval output = %s", got)
	}
	if nested, ok := done["e/1"]; !ok || nested.OK {
		t.Fatalf("nested call = %+v", nested)
	}
}
