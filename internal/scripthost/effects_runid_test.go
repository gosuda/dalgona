package scripthost_test

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
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
