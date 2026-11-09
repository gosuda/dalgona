package event_test

import (
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func openCall(t *testing.T, state event.State, callID string) event.State {
	t.Helper()
	var err error
	seq := uint64(2)
	state, err = event.Fold(state, seq, event.TurnStarted{TurnID: "t"})
	if err != nil {
		t.Fatalf("fold turn.started: %v", err)
	}
	assistant := event.AssistantMessage{
		ItemID: "a", TurnID: "t", StopReason: "tool_calls",
		ToolCalls: []event.ToolCall{{ID: callID, Name: "read", Args: jsontext.Value(`{}`)}},
	}
	state, err = event.Fold(state, seq+1, assistant)
	if err != nil {
		t.Fatalf("fold assistant.message: %v", err)
	}
	started := event.ToolStarted{
		CallID: callID, TurnID: "t", Tool: "read", Class: "read", Args: jsontext.Value(`{}`),
	}
	state, err = event.Fold(state, seq+2, started)
	if err != nil {
		t.Fatalf("fold tool.started: %v", err)
	}
	return state
}

func baseSession(t *testing.T) event.State {
	t.Helper()
	state, err := event.Fold(event.State{}, 1, event.SessionCreated{
		ID: "s", Cwd: "/w", Model: "m",
		Approval: "auto", Sandbox: "off", Runner: "local",
	})
	if err != nil {
		t.Fatalf("fold session.created: %v", err)
	}
	return state
}

func TestApplyIsAllOrNothingOnSnapshotFailure(t *testing.T) {
	t.Parallel()
	acc := openCall(t, baseSession(t), "c")
	poisoned := event.ToolCompleted{
		CallID: "c", OK: true, Recovered: "none", Meta: jsontext.Value(`{"captures":"nope"}`),
	}
	status := acc.Status
	if err := acc.Apply(5, poisoned); err == nil {
		t.Fatal("Apply accepted a completion with undecodable snapshot metadata")
	}
	acc.Status = status
	if _, ok := acc.InFlight["c"]; !ok {
		t.Fatal("failed Apply dropped the in-flight call instead of leaving the last good prefix")
	}
	if len(acc.CompletedTurns) != 0 || len(acc.Snapshots) != 0 {
		t.Fatalf("failed Apply mutated CompletedTurns=%v Snapshots=%v", acc.CompletedTurns, acc.Snapshots)
	}
	finished := event.ToolCompleted{CallID: "c", OK: true, Recovered: "none"}
	if err := acc.Apply(5, finished); err != nil {
		t.Fatalf("Apply after a failed event: %v", err)
	}
	if _, ok := acc.InFlight["c"]; ok {
		t.Fatal("valid completion left the call in flight")
	}
}

func TestApplyAssistantBatchIsAtomic(t *testing.T) {
	t.Parallel()
	state, err := event.Fold(baseSession(t), 2, event.TurnStarted{TurnID: "t"})
	if err != nil {
		t.Fatalf("fold turn.started: %v", err)
	}
	duplicated := event.AssistantMessage{
		ItemID: "a", TurnID: "t", StopReason: "tool_calls",
		ToolCalls: []event.ToolCall{
			{ID: "fresh", Name: "read", Args: jsontext.Value(`{}`)},
			{ID: "fresh", Name: "read", Args: jsontext.Value(`{}`)},
		},
	}
	if err := state.Apply(3, duplicated); err == nil {
		t.Fatal("Apply accepted a message with a duplicated call id")
	}
	if len(state.PendingCalls) != 0 {
		t.Fatalf("failed Apply kept %d pending calls", len(state.PendingCalls))
	}
}
