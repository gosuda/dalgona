package secrets_test

import (
	"io"
	"regexp"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/secrets"
)

func FuzzRegexpReaderFindAllParity(f *testing.F) {
	f.Add(`a|a.*z`, "abzaz")
	f.Add(`a?`, "é🙂a\xff")
	f.Add(`\b(?:\w+|)\b`, "a aa à _b")
	f.Add(`(?m)^.*?$`, "\nfoo\nbar\n\n")
	f.Add(`(a*?)*|\z`, "aaabaa")
	f.Add(`(a*?|b?)*`, "baa\n")
	f.Fuzz(func(t *testing.T, pattern, text string) {
		if len(text) > 1024 || len(pattern) > 256 {
			return
		}
		re, err := regexp.Compile(pattern)
		if err != nil {
			return
		}
		input := io.NewSectionReader(strings.NewReader(text), 0, int64(len(text)))
		reader, err := secrets.NewRegexpReader(t.Context(), input, pattern)
		if err != nil {
			t.Fatal(err)
		}
		checkRegexpSpans(t, reader, re.FindAllStringIndex(text, -1))
	})
}
