package agent

import (
	"bytes"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
)

func TestToolStartedStoresPostImage(t *testing.T) {
	h := newHarness(t, withApproval(config.ApprovalYolo))
	h.write("a.txt", "one\n")
	h.model.push(calls(call{id: "p", name: "patch", args: `{"changes":[{"path":"a.txt","old":"one","new":"uno"}]}`}), text("done"))
	thread := h.start()
	h.submit(thread, "edit")
	h.waitIdle(thread)

	var started event.ToolStarted
	for _, item := range h.events(thread.ID()) {
		if value, ok := item.Event.(event.ToolStarted); ok && value.CallID == "p" {
			started = value
		}
	}
	if len(started.Files) != 1 || started.Files[0].PostBlob == nil {
		t.Fatalf("tool.started files = %+v, want one file with post_blob", started.Files)
	}
	got, err := h.journal.Blob(started.Files[0].PostBlob.Blob)
	if err != nil {
		t.Fatalf("read post_blob: %v", err)
	}
	if !bytes.Equal(got, []byte("uno\n")) {
		t.Fatalf("post_blob = %q, want %q", got, "uno\n")
	}
}
