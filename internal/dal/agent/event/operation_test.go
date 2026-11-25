package event_test

import (
	"reflect"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

const operationID = "019a3b7c-1d2e-7f00-8a00-0123456789ab"

func stampedEffects(operation string) []event.Event {
	return []event.Event{
		event.SessionCreated{ID: "s", Cwd: "/w", Model: "m", Approval: "ask", Sandbox: "off", Runner: "local", Operation: operation},
		event.UserMessage{ItemID: "u", TurnID: "t", Text: "hi", Kind: "prompt", Operation: operation},
		event.TurnCompleted{TurnID: "t", Status: "interrupted", Operation: operation},
		event.ThreadNamed{Name: "renamed", Operation: operation},
		event.Compaction{FirstKeptSeq: 2, Summary: "summary", Method: "manual", TokensBefore: 10, TokensAfter: 1, Operation: operation},
		event.Branch{ParentSeq: 0, Operation: operation},
		event.ThreadArchived{Archived: true, Operation: operation},
	}
}

func foldStamped(t *testing.T, operation string) event.State {
	t.Helper()
	effects := stampedEffects(operation)
	state, err := foldEvents(t, append([]event.Event{effects[0], event.TurnStarted{TurnID: "t"}}, effects[1:]...)...)
	if err != nil {
		t.Fatalf("fold with operation id %q: %v", operation, err)
	}
	return state
}

func TestOperationIDTravelsWithItsEffectEvent(t *testing.T) {
	t.Parallel()
	for _, original := range stampedEffects(operationID) {
		encoded, err := event.Encode(original)
		if err != nil {
			t.Fatalf("Encode(%s): %v", original.Type(), err)
		}
		if !strings.Contains(string(encoded), `"operation_id":"`+operationID+`"`) {
			t.Fatalf("%s journal form %s lacks the operation_id member", original.Type(), encoded)
		}
		decoded, err := event.Decode(encoded)
		if err != nil || !reflect.DeepEqual(decoded, original) {
			t.Fatalf("%s round trip = %#v, %v; want %#v", original.Type(), decoded, err, original)
		}
		if got := event.OperationOf(decoded); got != operationID {
			t.Fatalf("OperationOf(%s) = %q, want %q", original.Type(), got, operationID)
		}
	}
}

func TestOperationIDLeavesTheFoldUnchanged(t *testing.T) {
	t.Parallel()
	stamped, plain := foldStamped(t, operationID), foldStamped(t, "")
	if stamped.LastCompaction == nil || stamped.LastCompaction.Operation != operationID {
		t.Fatalf("fold kept compaction %+v, want operation %s", stamped.LastCompaction, operationID)
	}
	unstamped := *stamped.LastCompaction
	unstamped.Operation, stamped.LastCompaction = "", &unstamped
	if !reflect.DeepEqual(stamped, plain) || stamped.Name != "renamed" || !stamped.Archived {
		t.Fatalf("fold with operation ids = %+v, without = %+v", stamped, plain)
	}
}

func TestOperationIDStaysOptional(t *testing.T) {
	t.Parallel()
	for _, raw := range []string{
		`{"type":"session.created","id":"s","cwd":"/w","model":"m","approval":"ask","sandbox":"off","runner":"local"}`,
		`{"type":"user.message","item_id":"u","turn_id":"t","text":"hi","kind":"prompt"}`,
		`{"type":"turn.completed","turn_id":"t","status":"interrupted"}`,
		`{"type":"thread.named","name":"old"}`,
		`{"type":"compaction","first_kept_seq":2,"summary":"summary","method":"manual","tokens_before":10,"tokens_after":1}`,
		`{"type":"branch","parent_seq":0}`,
		`{"type":"thread.archived","archived":true}`,
	} {
		decoded, err := event.Decode([]byte(raw))
		if err != nil {
			t.Fatalf("Decode(%s): %v", raw, err)
		}
		encoded, err := event.Encode(decoded)
		if err != nil || strings.Contains(string(encoded), "operation_id") || event.OperationOf(decoded) != "" {
			t.Fatalf("record without an operation re-encoded as %s, %v", encoded, err)
		}
	}
}

func TestOperationIDIsBounded(t *testing.T) {
	t.Parallel()
	long := strings.Repeat("o", event.MaxOperationBytes+1)
	for _, invalid := range stampedEffects(long) {
		if err := invalid.Validate(); err == nil {
			t.Fatalf("%s accepted a %d byte operation_id", invalid.Type(), len(long))
		}
	}
	for _, bounded := range stampedEffects(strings.Repeat("o", event.MaxOperationBytes)) {
		if err := bounded.Validate(); err != nil {
			t.Fatalf("%s rejected a %d byte operation_id: %v", bounded.Type(), event.MaxOperationBytes, err)
		}
	}
}
