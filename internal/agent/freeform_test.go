package agent

import (
	"os"
	"path/filepath"
	"testing"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/patch"
)

func customCall(id, name, input string) response {
	head := `{"type":"custom_tool_call","call_id":"` + id + `","name":"` + name + `","input":`
	body := sseEvent("response.output_item.added", `{"type":"response.output_item.added","output_index":1,"item":`+head+`""}}`) +
		sseEvent("response.output_item.done", `{"type":"response.output_item.done","output_index":1,"item":`+head+string(jsonString(input))+`}}`)
	return response{body: body + completed()}
}

func TestFreeformCallJournalsItsInputAsJSON(t *testing.T) {
	t.Parallel()
	const input = "*** Begin Patch\n*** Add File: made.txt\n+hello\n*** End Patch\n"
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.extraTools = []tool.Tool{patch.New("apply_patch")}
	})
	h.model.push(customCall("p1", "apply_patch", input), text("done"))
	thread := h.start()
	h.submit(thread, "add the file")
	h.waitIdle(thread)
	events := h.events(thread.ID())
	assertTurnsCompleted(t, events)
	if journaled := journaledCallArgs(t, events); journaled != input {
		t.Fatalf("journaled input = %q, want %q", journaled, input)
	}
	if !completedOutputs(events)["p1"].OK {
		t.Fatalf("patch output = %+v", completedOutputs(events)["p1"])
	}
	made, err := os.ReadFile(filepath.Join(h.cwd, "made.txt"))
	if err != nil || string(made) != "hello\n" {
		t.Fatalf("made.txt = %q, %v", made, err)
	}
}

func assertTurnsCompleted(t *testing.T, events []event.Seqd) {
	t.Helper()
	for _, item := range events {
		if done, ok := item.Event.(event.TurnCompleted); ok && done.Status != statusCompleted {
			t.Fatalf("turn status = %s: %s", done.Status, done.Error)
		}
	}
}

func journaledCallArgs(t *testing.T, events []event.Seqd) string {
	t.Helper()
	var journaled string
	for _, item := range events {
		if message, ok := item.Event.(event.AssistantMessage); ok && len(message.ToolCalls) == 1 {
			if err := json.Unmarshal(message.ToolCalls[0].Args, &journaled); err != nil {
				t.Fatalf("journaled args %s: %v", message.ToolCalls[0].Args, err)
			}
		}
	}
	return journaled
}
