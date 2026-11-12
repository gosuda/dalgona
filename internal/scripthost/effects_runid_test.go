package scripthost_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestReusedRunIDAcrossTurnsRunsFresh(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.model.push(evalCall("1"), text("done"), evalCall("2"), text("done"))
	thread := h.run("first")
	h.idle(thread)
	if err := thread.Submit(t.Context(), agent.Input{Kind: agent.InputPrompt, Text: "second"}); err != nil {
		t.Fatal(err)
	}
	events := h.idle(thread)
	got := outputs(events)["e"].Output
	if !strings.Contains(got, `"value":2`) {
		t.Fatalf("reused run across turns did not run fresh, got %s", got)
	}
	if strings.Contains(strings.ToLower(got), "duplicate run") {
		t.Fatalf("reused run across turns wrongly refused, got %s", got)
	}
}

func TestReusedRunIDWithinTurnIsRefused(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.model.push(evalCall("1"), evalCall("2"), text("done"))
	thread := h.run("first")
	events := h.idle(thread)
	got := outputs(events)["e"].Output
	if strings.Contains(got, `"value":2`) {
		t.Fatalf("duplicate run in same turn returned fresh result: %s", got)
	}
	if !strings.Contains(strings.ToLower(got), "duplicate run") {
		t.Fatalf("expected duplicate run id error, got %s", got)
	}
}

func evalOutputs(events []event.Seqd) []string {
	var out []string
	for _, item := range events {
		if completed, ok := item.Event.(event.ToolCompleted); ok && completed.CallID == "e" {
			out = append(out, completed.Output)
		}
	}
	return out
}

func TestRepeatedEvalWithinTurnReturnsRecordedOutcome(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.model.push(evalCall("print('ran')\n1"), evalCall("print('ran')\n1"), text("done"))
	thread := h.run("first")
	events := h.idle(thread)
	got := evalOutputs(events)
	if len(got) != 2 {
		t.Fatalf("tool.completed count for e = %d, want 2", len(got))
	}
	for i, output := range got {
		if !strings.Contains(output, `"value":1`) {
			t.Fatalf("output %d missing the recorded value: %s", i, output)
		}
	}
	if !strings.Contains(got[0], "ran") {
		t.Fatalf("first output missing its print: %s", got[0])
	}
	if strings.Contains(got[1], "ran") || strings.Contains(strings.ToLower(got[1]), "duplicate run") {
		t.Fatalf("second output repeated the run instead of returning the recorded outcome: %s", got[1])
	}
}

func TestRepeatedEvalKeepsRecordedFailure(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	code := "tools.exec(command = \"echo x >> log.txt\")\n1 // 0"
	h.model.push(evalCall(code), evalCall(code), text("done"))
	thread := h.run("first")
	got := evalOutputs(h.idle(thread))
	if len(got) != 2 {
		t.Fatalf("tool.completed count for e = %d, want 2", len(got))
	}
	if !strings.Contains(got[0], `"status":"failed"`) {
		t.Fatalf("first output is not the failed run: %s", got[0])
	}
	if got[1] != got[0] {
		t.Fatalf("second output did not replay the recorded failure:\nfirst: %s\nsecond: %s", got[0], got[1])
	}
	log, err := os.ReadFile(filepath.Join(h.cwd, "log.txt"))
	if err != nil || strings.Count(string(log), "x") != 1 {
		t.Fatalf("exec ran %d times, want 1: %q, %v", strings.Count(string(log), "x"), log, err)
	}
}
