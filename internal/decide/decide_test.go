package decide_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/decide"
)

func TestParse(t *testing.T) {
	t.Parallel()
	tests := []struct {
		input string
		want  decide.Spec
	}{
		{input: "decisions:typesafe/jev-1.14", want: decide.Spec{Adapter: decide.AdapterDecisions, Model: "typesafe/jev-1.14"}},
		{input: "llm:smol", want: decide.Spec{Adapter: decide.AdapterLLM, Model: "smol"}},
		{input: "llm:anthropic/claude-haiku-5-5", want: decide.Spec{Adapter: decide.AdapterLLM, Model: "anthropic/claude-haiku-5-5"}},
	}
	for _, test := range tests {
		got, err := decide.Parse(test.input)
		if err != nil {
			t.Fatalf("Parse(%q): %v", test.input, err)
		}
		if got != test.want {
			t.Errorf("Parse(%q) = %+v, want %+v", test.input, got, test.want)
		}
	}
}

func TestParseRejectsInvalidSpecs(t *testing.T) {
	t.Parallel()
	for _, input := range []string{"", "smol", "decisions:", "llm:", "http:model", ":model"} {
		if _, err := decide.Parse(input); err == nil {
			t.Errorf("Parse(%q) succeeded", input)
		}
	}
}
