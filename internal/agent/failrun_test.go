package agent

import (
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestFailBeforeRunJournalsDecline(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	events := runCalls(t, h, call{id: "x", name: "patch", args: `{"changes":[{"path":"missing.txt","old":"old","new":"new"}]}`})
	checkPreGateDecline(t, events)
}

func checkPreGateDecline(t *testing.T, events []event.Seqd) {
	t.Helper()
	if started(events, "x") {
		t.Fatal("a call that failed before the gate journaled tool.started")
	}
	resolved := resolutions(events)["x"]
	if resolved.Decision != "decline" || resolved.Reason == "" {
		t.Fatalf("failed pre-gate attempt has no declined resolution: %+v", resolved)
	}
	if completed := completedOutputs(events)["x"]; completed.OK || completed.Output == "" {
		t.Fatalf("completed = %+v", completed)
	}
}
