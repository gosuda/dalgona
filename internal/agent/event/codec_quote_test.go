package event_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestEncodeScoresWithControlCharacterKeys(t *testing.T) {
	t.Parallel()
	resolved := event.ApprovalResolved{
		RequestID: "r",
		Decision:  "accept",
		By:        "user",
		Reason:    "ok",
		Scores:    map[string]float64{"pre\x01\x07post": 0.5},
	}
	raw, err := event.Encode(resolved)
	if err != nil {
		t.Fatalf("Encode with control characters in score names: %v", err)
	}
	decoded, err := event.Decode(raw)
	if err != nil {
		t.Fatalf("Decode round trip: %v", err)
	}
	back, ok := decoded.(event.ApprovalResolved)
	if !ok {
		t.Fatalf("decoded type = %T, want ApprovalResolved", decoded)
	}
	if back.Scores["pre\x01\x07post"] != 0.5 {
		t.Fatalf("scores round trip = %#v", back.Scores)
	}
}
