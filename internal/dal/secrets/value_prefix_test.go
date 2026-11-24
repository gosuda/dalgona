package secrets_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dal/secrets"
)

func TestValuePrefixSettlesWholeMatchBytes(t *testing.T) {
	for _, row := range []struct {
		name, expression, window string
		safe                     int
	}{
		{"whole value, not capture", `(?m)^key=([A-Z]{5})$`, "xkey=ABCDE\n", 1},
		{"short ASCII branch", `a{7}|z{8}`, "aaaaaaa\n", 8},
		{"eight bytes in four runes", `é{4}`, "éééé\n", 0},
		{"unambiguous valid Unicode", `β{4}`, "é 🚀\n", len("é 🚀\n")},
		{"same value can start inside a rune", `\x{fffd}abcdefgh`, "éabcdefgh\n", 0},
		{"surrogate class endpoint keeps valid lower runes", `[a-\x{D800}]{8}`, "progress\n", 0},
		{"empty rune class admits progress", `[^\x00-\x{10FFFF}]`, "progress\n", 9},
		{"unfinished candidate", `old-[A-Z]{4,}`, "go\nold-AB", 3},
		{"only a capture is short", `(?:prefix-([a-z]))`, "xprefix-z\n", 1},
	} {
		t.Run(row.name, func(t *testing.T) {
			prefix, err := secrets.NewValuePrefix([]string{row.expression}, false)
			if err != nil {
				t.Fatal(err)
			}
			if got := prefix.SafeBytes([]byte(row.window), 0); got != row.safe {
				t.Fatalf("safe bytes = %d, want %d", got, row.safe)
			}
		})
	}
}

func TestValuePrefixDisprovesUnfinishedCandidateAcrossReads(t *testing.T) {
	prefix, err := secrets.NewValuePrefix([]string{`old-[A-Z]{4,}`}, false)
	if err != nil {
		t.Fatal(err)
	}
	if got := prefix.SafeBytes([]byte("go\nold-AB"), 0); got != 3 {
		t.Fatalf("unfinished candidate left %d safe bytes, want 3", got)
	}
	const suffix = "old-AB!\n"
	if got := prefix.SafeBytes([]byte(suffix), 3); got != len(suffix) {
		t.Fatalf("disproved candidate left %d safe bytes, want %d", got, len(suffix))
	}
}

func TestValuePrefixDoesNotFenceOrdinaryBuiltinUnicode(t *testing.T) {
	prefix, err := secrets.NewValuePrefix(nil, true)
	if err != nil {
		t.Fatal(err)
	}
	const text = "go 🚀\n"
	if got := prefix.SafeBytes([]byte(text[:4]), 0); got != 3 {
		t.Fatalf("partial rune left %d safe bytes, want 3", got)
	}
	if got := prefix.SafeBytes([]byte(text[3:]), 3); got != len(text)-3 {
		t.Fatalf("complete rune left %d safe bytes, want %d", got, len(text)-3)
	}
}
