package keywords

import "testing"

func TestSpansUseByteOffsets(t *testing.T) {
	table := []Keyword{{Word: "ultrathink"}}
	spans := Spans("é ultrathink", table)
	if len(spans) != 1 || spans[0].Start != 3 || spans[0].End != 13 {
		t.Fatalf("spans = %+v, want one span from byte 3 to 13", spans)
	}
}

func TestSpansBoundariesAndMasking(t *testing.T) {
	t.Parallel()
	table := []Keyword{{Word: "ultrathink"}}
	tests := []struct {
		name   string
		prompt string
		want   bool
	}{
		{name: "standalone", prompt: "ultrathink", want: true},
		{name: "comma", prompt: "ultrathink,", want: true},
		{name: "uppercase", prompt: "ULTRATHINK"},
		{name: "suffix", prompt: "ultrathinking"},
		{name: "property", prompt: "x.ultrathink"},
		{name: "method", prompt: "ultrathink()"},
		{name: "path", prompt: "/ultrathink"},
		{name: "sentence period", prompt: "ultrathink.", want: true},
		{name: "extension", prompt: "ultrathink.md"},
		{name: "one colon", prompt: "x:ultrathink", want: true},
		{name: "namespace", prompt: "x::ultrathink"},
		{name: "inline code", prompt: "`ultrathink`"},
		{name: "double inline code", prompt: "``ultrathink``"},
		{name: "fenced code", prompt: "```\nultrathink\n```"},
		{name: "tilde fence", prompt: "~~~text\nultrathink\n~~~"},
		{name: "indented fence", prompt: "   ```go\nultrathink\n   ```"},
		{name: "xml", prompt: "<system>ultrathink</system>"},
		{name: "nested xml", prompt: "<x>before <x>ultrathink</x></x>"},
		{name: "html comment", prompt: "<!-- ultrathink -->"},
		{name: "after code", prompt: "`code` ultrathink", want: true},
		{name: "unicode before", prompt: "éultrathink"},
		{name: "unicode separated", prompt: "é ultrathink", want: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			got := len(Spans(test.prompt, table)) == 1
			if got != test.want {
				t.Errorf("Spans(%q) matched = %v, want %v", test.prompt, got, test.want)
			}
		})
	}
}
