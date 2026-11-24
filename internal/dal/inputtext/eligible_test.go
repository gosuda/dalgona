package inputtext

import "testing"

func TestSearchableNeedsThreeScalarsInEveryTerm(t *testing.T) {
	cases := []struct {
		query string
		want  bool
	}{
		{"ab ", false},
		{"abc de", false},
		{" abc ", true},
		{"abc def", true},
		{"", false},
		{" \t\u3000", false},
		{"\u3000abc\u3000", true},
		{"ab\u00a0cde", false},
		{"ab\u2028cde", false},
		{"abc\u200bde", true},
		{"e\u0301e", true},
		{"\U0001F1F0\U0001F1F7", false},
		{"\uac00\ub098\ub2e4", true},
		{"\uff0fab", true},
	}
	for _, c := range cases {
		if got := Searchable(c.query); got != c.want {
			t.Errorf("Searchable(%+q) = %t, want %t", c.query, got, c.want)
		}
	}
}

func TestBlankIgnoresOnlyInvisibleScalarsAndCompleteEscapes(t *testing.T) {
	cases := []struct {
		text string
		want bool
	}{
		{"", true},
		{" \t\n\r\u00a0\u1680\u2000\u2028\u2029\u202f\u205f\u3000\u0085", true},
		{"\x00\x07\x7f\u009b", true},
		{"\u00ad\u034f\u061c\u115f\u180e\u200b\u200d\u2060\u2064\u3164\ufe0f\ufeff\uffa0\U0001BCA0\U000E0001\U000E0100\U000E0FFF", true},
		{"\x1b[2J\x1b]0;title\x07\x1bPq#0\x1b\\\x1b_x\x1b\\\x1b7 ", true},
		{"x", false},
		{" x ", false},
		{"\u0301", false},
		{"\ue000", false},
		{"\U000F0001", false},
		{"\ufffd", false},
		{"\u2800", false},
		{"\ufdd0", false},
		{"\uff0f", false},
		{"\x1b]0;title", false},
		{"\x1b[", false},
		{"\x1b]0;a\x1b[31m\x07", false},
		{"\x1b\u00e9", false},
		{"\xff", false},
	}
	for _, c := range cases {
		if got := Blank(c.text); got != c.want {
			t.Errorf("Blank(%+q) = %t, want %t", c.text, got, c.want)
		}
	}
}

func TestEscapeLengthFollowsTheRecognitionProfile(t *testing.T) {
	cases := []struct {
		name string
		text string
		want int
	}{
		{"csi", "\x1b[31mx", 5},
		{"csi with private parameters and intermediate", "\x1b[?25 qx", 7},
		{"csi without final", "\x1b[1;2", 0},
		{"csi broken by can", "\x1b[1\x18m", 0},
		{"csi with non-ascii final", "\x1b[1\u00e9", 0},
		{"osc with bel", "\x1b]0;\ud55c\uae00\x07x", 11},
		{"osc with st", "\x1b]8;;https://example.com\x1b\\x", 26},
		{"osc without payload", "\x1b]104\x07", 6},
		{"osc with letter selector", "\x1b]l title\x1b\\", 0},
		{"osc without selector", "\x1b];title\x07", 0},
		{"osc selector followed by text", "\x1b]0title\x07", 0},
		{"osc cannot borrow a later terminator", "\x1b]0;a\x1b[31m\x07", 0},
		{"osc broken by sub", "\x1b]0;a\x1a\x07", 0},
		{"osc without terminator", "\x1b]0;title", 0},
		{"dcs", "\x1bPq#0;2\x1b\\x", 9},
		{"dcs with parameters and intermediate", "\x1bP1$r\x1b\\", 7},
		{"dcs needs a header", "\x1bP\x1b\\", 0},
		{"dcs ignores bel", "\x1bPqa\x07", 0},
		{"apc", "\x1b_payload\x1b\\", 11},
		{"sos ignores bel", "\x1bXabc\x07", 0},
		{"pm", "\x1b^\x1b\\", 4},
		{"short escape", "\x1b7x", 2},
		{"short escape with intermediate", "\x1b(Bx", 3},
		{"string terminator alone", "\x1b\\", 2},
		{"short escape without final", "\x1b(", 0},
		{"non-ascii after escape", "\x1b\u00e9", 0},
		{"lone escape", "\x1b", 0},
		{"not an escape", "x\x1b[m", 0},
		{"payload with malformed utf-8", "\x1b]0;\xff\x07", 0},
	}
	for _, c := range cases {
		if got := EscapeLength(c.text); got != c.want {
			t.Errorf("%s: EscapeLength(%+q) = %d, want %d", c.name, c.text, got, c.want)
		}
	}
}
