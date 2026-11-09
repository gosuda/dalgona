package scripthost_test

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/script"
)

func TestDeniedTryCallRecordsAMarkedEffect(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withPolicy(t))
	h.model.push(evalCall("r = ctx.try_call(tools.exec, command = \"x\")\n42"), text("done"))
	parent := h.run("run the cell")
	done := outputs(h.idle(parent))
	if !strings.Contains(done["e"].Output, `"status":"denied"`) || strings.Contains(done["e"].Output, `"value":42`) {
		t.Fatalf("live eval output = %s", done["e"].Output)
	}
	var marked bool
	for _, item := range h.events(parent.ID()) {
		effect, ok := item.Event.(event.ScriptEffect)
		if !ok {
			continue
		}
		if !strings.HasPrefix(effect.Error, script.DeniedMark) {
			t.Fatalf("recorded denial = %q, want %q prefix", effect.Error, script.DeniedMark)
		}
		if !strings.Contains(effect.Error, "hooks forbid exec") {
			t.Fatalf("recorded denial = %q, want the hook reason", effect.Error)
		}
		marked = true
	}
	if !marked {
		t.Fatal("no script.effect recorded for the denied try_call")
	}
}
