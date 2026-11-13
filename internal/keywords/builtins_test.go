package keywords_test

import (
	"os"
	"path/filepath"
	"slices"
	"strings"
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
		{"dal", "decide", true},
		{"dal", "simplify", true},
		{"dalgona", "jevify", true},
		{"dalgona", "workflowz", true},
		{"dalgona", "orchestrate", true},
		{"dal", "ratchet", false},
		{"dalgona", "ratchet", true},
		{"dal", "review", true},
	}
	for _, test := range cases {
		if got := keywords.DefaultEnabled(test.product, test.word); got != test.want {
			t.Errorf("DefaultEnabled(%q, %q) = %v, want %v", test.product, test.word, got, test.want)
		}
	}
}

func TestEveryBuiltinHasItsNotice(t *testing.T) {
	t.Parallel()
	entries, err := os.ReadDir("notices")
	if err != nil {
		t.Fatalf("read notices: %v", err)
	}
	var files []string
	for _, entry := range entries {
		if stem, ok := strings.CutSuffix(entry.Name(), ".md"); ok {
			files = append(files, stem)
		}
	}
	var words []string
	for _, keyword := range keywords.Builtins("dalgona") {
		words = append(words, keyword.Word)
		data, err := os.ReadFile(filepath.Join("notices", keyword.Word+".md"))
		if err != nil {
			t.Errorf("%s has no notice file: %v", keyword.Word, err)
			continue
		}
		if keyword.Notice == "" || keyword.Notice != strings.TrimSpace(string(data)) {
			t.Errorf("%s notice is not the trimmed content of its file", keyword.Word)
		}
	}
	slices.Sort(files)
	slices.Sort(words)
	if !slices.Equal(files, words) {
		t.Errorf("notice files %v do not match built-ins %v", files, words)
	}
}
