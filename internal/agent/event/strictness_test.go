package event_test

import (
	"encoding/json/jsontext"
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
