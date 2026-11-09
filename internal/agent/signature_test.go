package agent

import (
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
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
