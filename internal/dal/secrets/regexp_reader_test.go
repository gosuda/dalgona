package secrets_test

import (
	"errors"
	"io"
	"regexp"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/secrets"
)

func TestRegexpReaderFindAllParity(t *testing.T) {
	const prefixBytes = (32 << 10) - 2
	prefix := strings.Repeat("x", prefixBytes)
	text := prefix + "🚀a café\nKEY=abcdefgh\nxxa🙂a aa KEY=abcdefgh\n" +
		strings.Repeat("a", 1024) + "\xff\xfe tail\nfinal"
	patterns := []string{
		`\Ax+|^KEY=[a-z]+|\bKEY=[a-z]+`, `(?m)^KEY=[a-z]+$|final\z`,
		`\ba+\b|\Ba+\B|\p{L}+`, `(?:🚀|🙂|\x{fffd})a?`,
		`a*|\b|$`, `(?:ab?)*?`, `a+Z|a{8}`, `a+Z|a{8}?`,
		`(?s)🚀.*?final\z`, `(?s)🚀.*final\z`, `foo|a|aa`, `aa|a|foo`,
		`x(?U:.*)KEY=abcdefgh`, `(?i:CAFÉ)|[0-9[:alpha:]]+`, `(?s:.)`, `a\z`,
	}
	for _, expression := range patterns {
		t.Run(expression, func(t *testing.T) {
			want := regexp.MustCompile(expression).FindAllStringIndex(text, -1)
			input := io.NewSectionReader(strings.NewReader(text), 0, int64(len(text)))
			reader, err := secrets.NewRegexpReader(t.Context(), input, expression)
			if err != nil {
				t.Fatal(err)
			}
			checkRegexpSpans(t, reader, want)
		})
	}
}

func checkRegexpSpans(t *testing.T, reader *secrets.RegexpReader, want [][]int) {
	t.Helper()
	for i, span := range want {
		start, end, err := reader.Next()
		if err != nil || !slices.Equal([]int{int(start), int(end)}, span) {
			t.Fatalf("match %d = [%d,%d), %v; want %v", i, start, end, err, span)
		}
	}
	if start, end, err := reader.Next(); !errors.Is(err, io.EOF) {
		t.Fatalf("after %d matches got [%d,%d), %v; want EOF", len(want), start, end, err)
	}
}

func TestRegexpReaderDistinguishesTruncationFromNoMatch(t *testing.T) {
	input := io.NewSectionReader(strings.NewReader("complete"), 0, 100)
	reader, err := secrets.NewRegexpReader(t.Context(), input, `secret.*\z`)
	if err != nil {
		t.Fatal(err)
	}
	if _, _, err := reader.Next(); !errors.Is(err, io.ErrUnexpectedEOF) {
		t.Fatalf("truncated input error = %v, want unexpected EOF", err)
	}
}
