package keywords

import "testing"

func TestSpansUseByteOffsets(t *testing.T) {
	table := []Keyword{{Word: "ultrathink"}}
	spans := Spans("é ultrathink", table)
	if len(spans) != 1 || spans[0].Start != 3 || spans[0].End != 13 {
		t.Fatalf("spans = %+v, want one span from byte 3 to 13", spans)
	}
}
