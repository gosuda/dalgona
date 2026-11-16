package scripthost_test

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

const nestedSpawnError = "agent: subagents cannot spawn subagents"

func spawnedChildren(events []event.Seqd) []string {
	var ids []string
	for _, item := range events {
		if spawned, ok := item.Event.(event.ChildSpawned); ok {
			ids = append(ids, spawned.ChildID)
		}
	}
	return ids
}

func TestChildThreadCannotSpawnSubagents(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.model.routes["child"] = []reply{evalCall(`ctx.spawn("grand")`), text("child done")}
	h.model.routes["grand"] = []reply{text("grand done")}
	h.model.push(evalCall(`ctx.wait([ctx.spawn("child")])`), text("done"))
	rootEvents := h.idle(h.run("run the cell"))
	children := spawnedChildren(rootEvents)
	if len(children) != 1 || !strings.Contains(outputs(rootEvents)["e"].Output, "child done") {
		t.Fatalf("root spawn: children = %v, eval output = %s", children, outputs(rootEvents)["e"].Output)
	}
	childEvents := h.events(children[0])
	if got := outputs(childEvents)["e"].Output; !strings.Contains(got, nestedSpawnError) {
		t.Fatalf("child cell output = %s, want %q", got, nestedSpawnError)
	}
	if nested := spawnedChildren(childEvents); len(nested) != 0 || h.model.requests("grand") != 0 {
		t.Fatalf("a child thread started %v, grand requests = %d", nested, h.model.requests("grand"))
	}
}
