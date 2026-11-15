package agent

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/tool"
)

func TestDuplicateToolNamesRefusedBeforePublication(t *testing.T) {
	t.Parallel()
	duplicate := stubTool{name: "read", class: tool.ClassRead}
	h := newHarness(t, withToolsByModel([]tool.Tool{duplicate, duplicate}, nil))
	parent := h.start()
	if err := parent.ensureTools(t.Context()); err == nil || !strings.Contains(err.Error(), `duplicate tool name "read"`) {
		t.Fatalf("duplicate registry = %v", err)
	}
	if parent.currentTools() != nil {
		t.Fatal("duplicate registry was published")
	}
}

func TestFixedChildToolsRespectParentCeiling(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	parent := h.start()
	seedSpawnCalls(t, h, parent, "fixed", 1)
	if err := parent.Append(t.Context(), event.ChildSpawned{CallID: "fixed-0", ChildID: "fixed-child", Runner: "local", Tools: []string{"read", "yield"}}); err != nil {
		t.Fatal(err)
	}
	child, err := h.host.startSession(t.Context(), ThreadSpec{Parent: parent.ID(), Overrides: Overrides{Tools: []tool.Tool{
		stubTool{name: "read", class: tool.ClassRead}, stubTool{name: "exec", class: tool.ClassExec}, yieldTool{},
	}}}, "fixed-child")
	if err != nil {
		t.Fatal(err)
	}
	if err := child.ensureTools(t.Context()); err != nil {
		t.Fatal(err)
	}
	set := child.currentTools()
	if _, found := set.lookup("exec"); found {
		t.Fatal("fixed tools bypassed the ceiling")
	}
	if _, found := set.lookup("read"); !found {
		t.Fatal("allowed fixed tool disappeared")
	}
}

func TestChildDispatchRechecksCeiling(t *testing.T) {
	t.Parallel()
	set := &toolset{
		byName: map[string]tool.Tool{
			"read": stubTool{name: "read", class: tool.ClassRead},
			"exec": stubTool{name: "exec", class: tool.ClassExec},
		},
		allowed: []string{"read", "yield"},
	}
	if _, found := set.lookup("exec"); found {
		t.Fatal("dispatch accepted a tool outside the immutable ceiling")
	}
	if _, found := set.lookup("read"); !found {
		t.Fatal("dispatch refused a tool inside the immutable ceiling")
	}
}
