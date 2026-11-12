package agent

import (
	"encoding/json/jsontext"
	"reflect"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/model"
)

func TestTranscriptRebuildsSignedToolCalls(t *testing.T) {
	b := &transcript{
		names:       map[string]string{},
		order:       map[string]int{},
		outstanding: map[string]bool{},
	}
	b.assistant(event.AssistantMessage{
		ItemID: "a", TurnID: "t",
		ToolCalls: []event.ToolCall{{
			ID: "c", Name: "exec", Args: jsontext.Value(`{"command":"ls"}`), Signature: "sig-abc",
		}},
		StopReason: "tool_calls",
	})
	if b.err != nil {
		t.Fatalf("rebuild: %v", b.err)
	}
	if len(b.messages) != 1 || len(b.messages[0].ToolCalls) != 1 {
		t.Fatalf("messages = %#v", b.messages)
	}
	call := b.messages[0].ToolCalls[0]
	if call.Signature != "sig-abc" {
		t.Fatalf("signature = %q, want sig-abc", call.Signature)
	}
}

func TestTranscriptRebuildsRedactedThinking(t *testing.T) {
	b := &transcript{
		names:       map[string]string{},
		order:       map[string]int{},
		outstanding: map[string]bool{},
	}
	b.assistant(event.AssistantMessage{
		ItemID: "a", TurnID: "t",
		ToolCalls: []event.ToolCall{{
			ID: "c", Name: "exec", Args: jsontext.Value(`{"command":"ls"}`), RedactedThinking: []string{"enc-1"},
		}},
		StopReason: "tool_calls",
	})
	if b.err != nil {
		t.Fatalf("rebuild: %v", b.err)
	}
	if len(b.messages) != 1 || len(b.messages[0].ToolCalls) != 1 {
		t.Fatalf("messages = %#v", b.messages)
	}
	if got := b.messages[0].ToolCalls[0].RedactedThinking; !reflect.DeepEqual(got, []string{"enc-1"}) {
		t.Fatalf("redacted thinking = %q, want enc-1", got)
	}
}

func TestApplyKeepsRedactedThinking(t *testing.T) {
	result := &reply{}
	err := (&worker{}).apply(result, model.ToolCall{
		ID: "c", Name: "read", Args: jsontext.Value(`{}`), RedactedThinking: []string{"enc-1"},
	})
	if err != nil {
		t.Fatalf("apply: %v", err)
	}
	if len(result.calls) != 1 || !reflect.DeepEqual(result.calls[0].RedactedThinking, []string{"enc-1"}) {
		t.Fatalf("calls = %#v, want redacted thinking enc-1", result.calls)
	}
}
