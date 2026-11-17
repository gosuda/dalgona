package search

import "testing"

func TestSearchWireUsesJSONControlEscapes(t *testing.T) {
	want := outcome{Text: "a\x01b"}
	encoded := encodeOutcome(want)
	got, err := decodeOutcome(encoded)
	if err != nil {
		t.Fatalf("wire is not JSON: %v\n%s", err, encoded)
	}
	if got.Text != want.Text {
		t.Fatalf("text %q, want %q", got.Text, want.Text)
	}
}
