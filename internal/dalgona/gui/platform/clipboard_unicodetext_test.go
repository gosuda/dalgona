package platform

import (
	"errors"
	"slices"
	"testing"
	"unicode/utf16"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

func unicodeTextBlock(text string) []uint16 {
	return append(utf16.Encode([]rune(text)), 0)
}

func TestUnicodeTextKeepsEveryScalarBeforeTheTerminator(t *testing.T) {
	for _, text := range []string{
		"",
		"first line\r\nsecond line\r\n",
		"e\u0301 \u1100\u1161\u11a8 \U0001F469\u200D\U0001F4BB",
		"\U0001F600",
		"\t\u2028\u00A0",
	} {
		got, err := decodeUnicodeText(unicodeTextBlock(text))
		if err != nil || got != text {
			t.Errorf("decodeUnicodeText(%q) = %q, %v", text, got, err)
		}
	}
	got, err := decodeUnicodeText([]uint16{0xD83D, 0xDE00, 0x000D, 0x000A, 0})
	if err != nil || got != "\U0001F600\r\n" {
		t.Errorf("the code units D83D DE00 000D 000A decoded to %q, %v; want U+1F600 CR LF", got, err)
	}
}

func TestUnicodeTextTreatsNULsAfterTheTerminatorAsPadding(t *testing.T) {
	cases := []struct {
		units []uint16
		want  string
	}{
		{[]uint16{'a', 'b', 0, 0, 0, 0}, "ab"},
		{[]uint16{'a', 0, 0}, "a"},
		{[]uint16{0, 0, 0}, ""},
		{[]uint16{0xD83D, 0xDE00, '\r', '\n', 0, 0}, "\U0001F600\r\n"},
	}
	for _, tc := range cases {
		got, err := decodeUnicodeText(tc.units)
		if err != nil || got != tc.want {
			t.Errorf("decodeUnicodeText(%04X) = %q, %v; want %q", tc.units, got, err, tc.want)
		}
	}
}

func TestUnicodeTextRefusesMalformedBlocks(t *testing.T) {
	cases := []struct {
		name  string
		units []uint16
		want  error
	}{
		{"text after the terminator", []uint16{'a', 0, 'b', 0}, textinput.DisallowedControl},
		{"text after terminator padding", []uint16{'a', 0, 0, 0, 'b', 0}, textinput.DisallowedControl},
		{"text in the last unit after a NUL", []uint16{'a', 0, 'b'}, textinput.DisallowedControl},
		{"no terminator", []uint16{'a', 'b'}, textinput.InvalidEncoding},
		{"no code units", nil, textinput.InvalidEncoding},
		{"high surrogate before a letter", []uint16{0xD83D, 'a', 0}, textinput.InvalidEncoding},
		{"high surrogate before the terminator", []uint16{'a', 0xD83D, 0}, textinput.InvalidEncoding},
		{"lone low surrogate", []uint16{0xDE00, 'a', 0}, textinput.InvalidEncoding},
		{"reversed surrogate pair", []uint16{0xDE00, 0xD83D, 0}, textinput.InvalidEncoding},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, err := decodeUnicodeText(tc.units)
			if !errors.Is(err, tc.want) || got != "" {
				t.Fatalf("decodeUnicodeText(%04X) = %q, %v; want %v", tc.units, got, err, tc.want)
			}
		})
	}
}

func TestUnicodeTextBoundsDecodedBytesBeforeDecoding(t *testing.T) {
	const (
		pairs  = 6291455
		triple = 2097152
		double = 1048576
		single = transferLimit - 4*pairs - 3*triple - 2*double
	)
	over := slices.Concat(
		[]uint16{0xDE00, 0xD83D},
		slices.Repeat([]uint16{0xD83D, 0xDE00}, pairs-1),
		slices.Repeat([]uint16{0x4E00}, triple),
		slices.Repeat([]uint16{0x00E9}, double),
		slices.Repeat([]uint16{'a'}, single),
		[]uint16{'a', 0},
	)
	if got, err := decodeUnicodeText(over); !errors.Is(err, ErrTransferTooLarge) || got != "" {
		t.Fatalf("text one UTF-8 byte past 32 MiB returned %d bytes, %v; want the size refusal", len(got), err)
	}
	exact := over[:len(over)-1]
	exact[len(exact)-1] = 0
	if got, err := decodeUnicodeText(exact); !errors.Is(err, textinput.InvalidEncoding) || got != "" {
		t.Fatalf("text of exactly 32 MiB that opens with a reversed surrogate pair returned %d bytes, %v; want the size bound to pass and decoding to refuse the pair", len(got), err)
	}
}

func TestUnicodeTextUnitsBoundGlobalSize(t *testing.T) {
	cases := []struct {
		size  uintptr
		units uintptr
		want  error
	}{
		{size: 2, units: 1},
		{size: transferLimit, units: transferLimit / 2},
		{size: transferLimit + 2, want: ErrTransferTooLarge},
		{size: 7, want: textinput.InvalidEncoding},
	}
	for _, tc := range cases {
		units, err := unicodeTextUnits(tc.size)
		if units != tc.units || !errors.Is(err, tc.want) {
			t.Errorf("unicodeTextUnits(%d) = %d, %v; want %d, %v", tc.size, units, err, tc.units, tc.want)
		}
	}
}
