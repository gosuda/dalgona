package event_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestFoldDeferredBlobbedExecUsesCarriedPrefix(t *testing.T) {
	t.Parallel()
	assistant := event.AssistantMessage{
		ItemID: "a", TurnID: "t",
		ToolCalls:  []event.ToolCall{{ID: "c", Name: "exec", ArgsBlob: &event.BlobRef{Blob: digest, Bytes: 70000}}},
		StopReason: "tool_calls",
	}
	state, err := foldEvents(t, event.State{},
		event.TurnStarted{TurnID: "t"},
		assistant,
		event.ApprovalRequested{RequestID: "r", CallID: "c", Summary: "big", GrantPrefix: "git"},
		event.ApprovalResolved{RequestID: "r", Decision: "accept_session", By: "user", Reason: "ok"},
		event.ToolStarted{CallID: "c", TurnID: "t", Tool: "exec", Class: "exec", ArgsBlob: &event.BlobRef{Blob: digest, Bytes: 70000}},
	)
	if err != nil {
		t.Fatalf("fold: %v", err)
	}
	if state.Grants["git"] != "exec" {
		t.Fatalf("grants = %#v, want a git prefix grant for exec", state.Grants)
	}
}

func TestFoldDeferredBlobbedExecWithoutPrefixGrantsNothing(t *testing.T) {
	t.Parallel()
	assistant := event.AssistantMessage{
		ItemID: "a", TurnID: "t",
		ToolCalls:  []event.ToolCall{{ID: "c", Name: "exec", ArgsBlob: &event.BlobRef{Blob: digest, Bytes: 70000}}},
		StopReason: "tool_calls",
	}
	state, err := foldEvents(t, event.State{},
		event.TurnStarted{TurnID: "t"},
		assistant,
		event.ApprovalRequested{RequestID: "r", CallID: "c", Summary: "big"},
		event.ApprovalResolved{RequestID: "r", Decision: "accept_session", By: "user", Reason: "ok"},
		event.ToolStarted{CallID: "c", TurnID: "t", Tool: "exec", Class: "exec", ArgsBlob: &event.BlobRef{Blob: digest, Bytes: 70000}},
	)
	if err != nil {
		t.Fatalf("fold: %v", err)
	}
	if len(state.Grants) != 0 {
		t.Fatalf("grants = %#v, want no grant", state.Grants)
	}
}
