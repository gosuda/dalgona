package script_test

import (
	"encoding/json/jsontext"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/tool"
)

func TestToolCallFailureIsPerHook(t *testing.T) {
	t.Parallel()
	plugins := loadPlugins(t, map[string]string{
		"aa.star": "def bad(ctx, event):\n    return {\"decision\": \"deny\"}\non('tool_call', bad)\n",
		"bb.star": "def guard(ctx, event):\n    return {\"decision\": \"deny\", \"reason\": \"second hook denies\"}\non('tool_call', guard)\n",
	})
	j := newJournal(echoTool)
	got, err := plugins.ToolCall(t.Context(), j, "c1", tool.Call{ID: "c1", Name: "exec", Args: jsontext.Value(`{"command":"ls"}`)})
	if err == nil || !strings.Contains(err.Error(), "hook:aa.star:bad") {
		t.Fatalf("joined error = %v", err)
	}
	if got.Decision != "deny" || got.Reason != "second hook denies" {
		t.Fatalf("decision = %+v", got)
	}
	if !strings.Contains(got.By, "hook:bb.star:guard") {
		t.Fatalf("by = %q", got.By)
	}
}
