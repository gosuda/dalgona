package agent

import (
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/rewrite"
)

func TestRewriteStartedHoldsPreImage(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalAuto), func(h *harness) { h.extraTools = []tool.Tool{rewrite.New()} })
	const source = "package a\n\nfunc Old() {}\n"
	h.write("a.go", source)
	events := runCalls(t, h, call{
		id:   "r",
		name: "rewrite",
		args: `{"lang":"go","pattern":"((identifier) @name (#eq? @name \"Old\"))","target":"name","replace":"New"}`,
	})
	var started *event.ToolStarted
	for _, item := range events {
		if value, ok := item.Event.(event.ToolStarted); ok && value.CallID == "r" {
			started = &value
		}
	}
	if started == nil || len(started.Files) != 1 || started.Files[0].PreBlob == nil {
		t.Fatalf("tool.started for rewrite has no pre-image: %+v", started)
	}
	pre, err := h.journal.Blob(started.Files[0].PreBlob.Blob)
	if err != nil {
		t.Fatal(err)
	}
	if string(pre) != source {
		t.Fatalf("pre-image = %q, want %q", pre, source)
	}
}
