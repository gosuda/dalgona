package keywords

import (
	"slices"
	"testing"
)

func TestPreviewableKeepsRequiredToolKeywords(t *testing.T) {
	table := []Keyword{
		{Word: "plain", Enabled: true},
		{Word: "needs", Enabled: true, Requires: []string{"eval"}},
		{Word: "off", Enabled: false},
		{Word: "Upper", Enabled: true},
	}
	var words []string
	for _, item := range Previewable(table) {
		words = append(words, item.Word)
	}
	if want := []string{"plain", "needs"}; !slices.Equal(words, want) {
		t.Fatalf("Previewable words = %v, want %v", words, want)
	}
	if usable := Usable(table, nil); len(usable) != 1 || usable[0].Word != "plain" {
		t.Fatalf("Usable without tools = %+v, want only plain", usable)
	}
}
