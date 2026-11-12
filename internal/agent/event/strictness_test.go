package event_test

import (
	"bytes"
	"encoding/json/jsontext"
	"reflect"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestDecodeRejectsMissingRequiredMembers(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		raw  string
	}{
		{
			name: "assistant.message usage",
			raw:  `{"type":"assistant.message","item_id":"a","turn_id":"t","text":"","reasoning":"","tool_calls":[],"stop_reason":"end_turn"}`,
		},
		{
			name: "approval.resolved reason",
			raw:  `{"type":"approval.resolved","request_id":"r","decision":"accept","by":"user"}`,
		},
		{
			name: "compaction tokens_before",
			raw:  `{"type":"compaction","first_kept_seq":1,"summary":"s","method":"auto","tokens_after":10}`,
		},
		{
			name: "compaction tokens_after",
			raw:  `{"type":"compaction","first_kept_seq":1,"summary":"s","method":"auto","tokens_before":10}`,
		},
		{
			name: "model.retry status",
			raw:  `{"type":"model.retry","turn_id":"t","attempt":1,"verdict":"retry","delay_ms":100}`,
		},
	}
	for _, tc := range cases {
		if _, err := event.Decode([]byte(tc.raw)); err == nil {
			t.Fatalf("%s: decode accepted a record missing a required member", tc.name)
		}
	}
}

func TestFoldAcceptsFarOutOfOrderScriptEffects(t *testing.T) {
	t.Parallel()
	script := event.ScriptStarted{RunID: "s", SourceBlob: event.BlobRef{Blob: digest, Bytes: 1}, Entry: "main", Args: jsontext.Value(`{}`)}
	effect := func(n uint64) event.Event {
		return event.ScriptEffect{RunID: "s", N: n, Op: "tool", ArgsSHA: digest, Result: jsontext.Value(`1`)}
	}
	state, err := foldEvents(t, event.State{}, event.TurnStarted{TurnID: "t"}, script, effect(0), effect(4096))
	if err != nil {
		t.Fatalf("fold rejected a legal out-of-order effect: %v", err)
	}
	if state.Status == event.StatusDamaged {
		t.Fatal("session marked damaged for a legal journal")
	}
	if _, err := event.Fold(state, 99, effect(4096)); err == nil {
		t.Fatal("duplicate effect index accepted")
	}
}

func TestFoldAcceptSessionBlobbedExecCreatesGrant(t *testing.T) {
	t.Parallel()
	assistant := event.AssistantMessage{
		ItemID: "a", TurnID: "t",
		ToolCalls:  []event.ToolCall{{ID: "c", Name: "exec", ArgsBlob: &event.BlobRef{Blob: digest, Bytes: 70000}}},
		StopReason: "tool_calls",
	}
	state, err := foldEvents(t, event.State{},
		event.TurnStarted{TurnID: "t"},
		assistant,
		event.ApprovalRequested{RequestID: "r", CallID: "c", Summary: "big", Class: "exec", GrantPrefix: "git"},
		event.ApprovalResolved{RequestID: "r", Decision: "accept_session", By: "user", Reason: "ok"},
	)
	if err != nil {
		t.Fatalf("fold: %v", err)
	}
	if state.Grants["git"] != "exec" {
		t.Fatalf("grants = %#v, want a git prefix grant for exec", state.Grants)
	}
}

func TestToolCallSignatureRoundTrip(t *testing.T) {
	t.Parallel()
	original := event.AssistantMessage{
		ItemID: "a", TurnID: "t",
		ToolCalls:  []event.ToolCall{{ID: "c", Name: "exec", Args: jsontext.Value(`{"command":"ls"}`), Signature: "sig-abc"}},
		Usage:      event.Usage{Input: 1},
		StopReason: "tool_calls",
	}
	encoded, err := event.Encode(original)
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	if !bytes.Contains(encoded, []byte(`"signature"`)) {
		t.Fatalf("encoded %s omits signature", encoded)
	}
	decoded, err := event.Decode(encoded)
	if err != nil {
		t.Fatalf("decode: %v", err)
	}
	if !reflect.DeepEqual(decoded, event.Event(original)) {
		t.Fatalf("round trip mismatch: %#v", decoded)
	}
	plain := event.AssistantMessage{
		ItemID: "a", TurnID: "t",
		ToolCalls:  []event.ToolCall{{ID: "c", Name: "exec", Args: jsontext.Value(`{"command":"ls"}`)}},
		Usage:      event.Usage{Input: 1},
		StopReason: "tool_calls",
	}
	encoded, err = event.Encode(plain)
	if err != nil {
		t.Fatalf("encode plain: %v", err)
	}
	if bytes.Contains(encoded, []byte(`"signature"`)) {
		t.Fatalf("encoded %s emits an empty signature", encoded)
	}
}

func TestToolCallRedactedThinkingRoundTrip(t *testing.T) {
	t.Parallel()
	original := event.AssistantMessage{
		ItemID: "a", TurnID: "t",
		ToolCalls:  []event.ToolCall{{ID: "c", Name: "exec", Args: jsontext.Value(`{"command":"ls"}`), RedactedThinking: []string{"enc-1", "enc-2"}}},
		Usage:      event.Usage{Input: 1},
		StopReason: "tool_calls",
	}
	encoded, err := event.Encode(original)
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	if !bytes.Contains(encoded, []byte(`"redacted_thinking":["enc-1","enc-2"]`)) {
		t.Fatalf("encoded %s omits redacted_thinking", encoded)
	}
	decoded, err := event.Decode(encoded)
	if err != nil {
		t.Fatalf("decode: %v", err)
	}
	if !reflect.DeepEqual(decoded, event.Event(original)) {
		t.Fatalf("round trip mismatch: %#v", decoded)
	}
	empty := original
	empty.ToolCalls = []event.ToolCall{{ID: "c", Name: "exec", Args: jsontext.Value(`{"command":"ls"}`), RedactedThinking: []string{""}}}
	if err := empty.Validate(); err == nil || !strings.Contains(err.Error(), "redacted_thinking[0] must not be empty") {
		t.Fatalf("validate empty redacted_thinking: %v", err)
	}
}
