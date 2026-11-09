package keywords_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/keywords"
)

func TestDefaultEnabled(t *testing.T) {
	t.Parallel()
	cases := []struct {
		product, word string
		want          bool
	}{
		{"dal", "ultrathink", true},
		{"dal", "jevify", false},
		{"dal", "workflowz", false},
		{"dal", "orchestrate", false},
		{"dalgona", "jevify", true},
		{"dalgona", "workflowz", true},
		{"dalgona", "orchestrate", true},
		{"dal", "review", true},
	}
	for _, test := range cases {
		if got := keywords.DefaultEnabled(test.product, test.word); got != test.want {
			t.Errorf("DefaultEnabled(%q, %q) = %v, want %v", test.product, test.word, got, test.want)
		}
	}
}
