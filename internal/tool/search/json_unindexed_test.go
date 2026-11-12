package search

import (
	"strings"
	"testing"
)

func TestOutcomeWireCarriesUnindexed(t *testing.T) {
	encoded := encodeOutcome(outcome{Text: "x", Unindexed: 1234})
	if !strings.Contains(string(encoded), `"unindexed":1234`) {
		t.Fatalf("encoded outcome %s lacks the unindexed member", encoded)
	}
	decoded, err := decodeOutcome(encoded)
	if err != nil {
		t.Fatal(err)
	}
	if decoded.Unindexed != 1234 {
		t.Fatalf("decoded Unindexed = %d, want 1234", decoded.Unindexed)
	}
}
