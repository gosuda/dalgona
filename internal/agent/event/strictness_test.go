package event_test

import (
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

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
