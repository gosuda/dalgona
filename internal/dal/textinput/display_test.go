package textinput

import (
	json "encoding/json/v2"
	"strings"
	"testing"
)

func TestEscapeEndFollowsTheRecognitionProfile(t *testing.T) {
	cases := []struct {
		name string
		text string
		end  int
		ok   bool
	}{
		{"CSI", "\x1b[1;31mx", 7, true},
		{"CSI with intermediate", "\x1b[1 qx", 5, true},
		{"CSI parameter after intermediate", "\x1b[ 1m", 0, false},
		{"incomplete CSI", "\x1b[12", 0, false},
		{"CSI with Hangul for its final byte", "\x1b[1\ud55c", 0, false},
		{"DCS", "\x1bPq#0;2\x1b\\x", 9, true},
		{"DCS with an empty payload", "\x1bPq\x1b\\", 5, true},
		{"DCS with Hangul before its final byte", "\x1bP\ud55c\x1b\\", 0, false},
		{"DCS cut by a non-ST ESC", "\x1bPq\x1b[1m\x1b\\", 0, false},
		{"DCS cut by CAN", "\x1bPq\x18\x1b\\", 0, false},
		{"DCS cut by SUB", "\x1bPq\x1a\x1b\\", 0, false},
		{"DCS never ends with BEL", "\x1bPqab\x07", 0, false},
		{"OSC with BEL", "\x1b]0;title\x07x", 10, true},
		{"OSC with ST", "\x1b]8;;\x1b\\", 7, true},
		{"OSC with an empty payload", "\x1b]2;\x07", 5, true},
		{"OSC without a payload", "\x1b]104\x07", 6, true},
		{"OSC with Hangul for its selector", "\x1b]\ud55c\x07", 0, false},
		{"OSC with a legacy letter selector", "\x1b]ltitle\x1b\\", 0, false},
		{"OSC without a selector before its payload", "\x1b];title\x07", 0, false},
		{"OSC without a selector", "\x1b]\x07", 0, false},
		{"unterminated OSC", "\x1b]0;title", 0, false},
		{"APC", "\x1b_payload\x1b\\", 11, true},
		{"PM", "\x1b^\x1b\\", 4, true},
		{"SOS keeps scalars whole", "\x1bX\ud55c\x1b\\", 7, true},
		{"short escape", "\x1b7x", 2, true},
		{"short escape with intermediate", "\x1b(Bx", 3, true},
		{"ESC before Hangul", "\x1b\ud55c", 0, false},
		{"ESC before a supplementary scalar", "\x1b\U00020000", 0, false},
		{"ESC before a combining mark", "\x1b\u0301", 0, false},
		{"ESC before private use", "\x1b\U000F0001", 0, false},
		{"lone ESC", "\x1b", 0, false},
	}
	for _, c := range cases {
		end, ok := EscapeEnd(c.text, 0)
		if end != c.end || ok != c.ok {
			t.Errorf("%s: EscapeEnd = %d, %v; want %d, %v", c.name, end, ok, c.end, c.ok)
		}
	}
}

func TestEligibleIgnoresOnlyInvisibleScalarsAndCompleteEscapes(t *testing.T) {
	blank := []string{"", " \u00a0\u3000\t\n", "\x1b[1m\x1b]0;t\x07", "\u202e\u2066", "\u200d\ufe0f", "\x00\x7f\u0085", "\x1bPq\x1b\\", "\x1b"}
	for _, text := range blank {
		if Eligible(text) {
			t.Errorf("Eligible(%q) = true, want false", text)
		}
	}
	visible := []string{" x ", "\u200dx", "\x1b[12", "\U000F0001", "\ufffd", "\ud55c"}
	for _, text := range visible {
		if !Eligible(text) {
			t.Errorf("Eligible(%q) = false, want true", text)
		}
	}
}

func TestDisplayLineEscapesWhatItCannotShow(t *testing.T) {
	cases := map[string]string{
		"  a\tb\nc  ":                  "a b c",
		"a\r\nb":                       "a b",
		"a\u2028b\u0085c":              "a b c",
		"\x1b[31mred\x1b[0m":           "red",
		"\x1b\ud55c":                   "<U+001B>\ud55c",
		"\x1b]0;title text":            "<U+001B>]0;title text",
		"\x1b[1\ud55c":                 "<U+001B>[1\ud55c",
		"\x1bPq\x1b[1mtext\x1b\\":      "<U+001B>Pqtext",
		"\x1b]\ud55c\x07x":             "<U+001B>]\ud55c<U+0007>x",
		"A\u202eB\u202cC":              "A<U+202E>B<U+202C>C",
		"\x00\x7f\u009b":               "<U+0000><U+007F><U+009B>",
		"\ue000\U000F0001\ufdd0\ufffd": "\ue000\U000F0001\ufdd0\ufffd",
		"e\u0301 \u1100\u1161":         "e\u0301 \u1100\u1161",
	}
	for text, want := range cases {
		if got := DisplayLine(text); got != want {
			t.Errorf("DisplayLine(%q) = %q, want %q", text, got, want)
		}
	}
}

func TestDisplayBlockKeepsLinesAndExpandsTabs(t *testing.T) {
	if got, want := DisplayBlock("\n \n\tcode\r\n\x1b[1mnext\u2028end  "), "    code\nnext\nend"; got != want {
		t.Fatalf("DisplayBlock = %q, want %q", got, want)
	}
}

func TestExposedShowsEveryHiddenDifference(t *testing.T) {
	cases := map[string]string{
		"Yes":              "Yes",
		"Yes\u200b":        "Yes<U+200B>",
		"\u00e9":           "<U+00E9>",
		"e\u0301":          "e<U+0301>",
		"<U+00E9>":         "<U+003C>U+00E9>",
		"a b":              "a<U+0020>b",
		"\x1b[1mYes":       "<U+001B>[1mYes",
		"\u0430pple":       "<U+0430>pple",
		"\U0001F468\u200d": "<U+1F468><U+200D>",
	}
	for text, want := range cases {
		if got := Exposed(text); got != want {
			t.Errorf("Exposed(%q) = %q, want %q", text, got, want)
		}
	}
}

func TestQuoteEscapesLineBreaksAndControlsWithoutChangingText(t *testing.T) {
	text := "  a\r\nb\u2028c\u0085d\x1b[2J\u202ee\"\\\u00a0\ue000\U0001F469\u200d\U0001F4BB  "
	quoted := Quote(text)
	for _, hidden := range []string{"\r", "\n", "\u2028", "\u0085", "\x1b", "\u202e"} {
		if strings.Contains(quoted, hidden) {
			t.Fatalf("Quote(%q) = %q, which still holds %q", text, quoted, hidden)
		}
	}
	var decoded string
	if err := json.Unmarshal([]byte(quoted), &decoded); err != nil || decoded != text {
		t.Fatalf("Quote(%q) = %q, which decodes to %q, %v", text, quoted, decoded, err)
	}
}

func TestEscapedShowsEveryControlButKeepsLayout(t *testing.T) {
	cases := map[string]string{
		"a\tb\nc":            "a\tb\nc",
		"\x1b[31mred":        "<U+001B>[31mred",
		"a\rb\u0085c\u2028d": "a<U+000D>b<U+0085>c<U+2028>d",
		"A\u202eB\u2066C":    "A<U+202E>B<U+2066>C",
		"\x00\x7f":           "<U+0000><U+007F>",
		"\ue000\U000F0001\ufeff\ufffd e\u0301 \u1100\u1161": "\ue000\U000F0001\ufeff\ufffd e\u0301 \u1100\u1161",
	}
	for text, want := range cases {
		if got := Escaped(text); got != want {
			t.Errorf("Escaped(%q) = %q, want %q", text, got, want)
		}
	}
}

func TestDistinctLinesExposeOnlyCollidingLabels(t *testing.T) {
	got := DistinctLines([]string{"Yes", "Yes\u200b", "No", " No ", "Maybe", "\x1b[1mMaybe\x1b[0m"})
	want := []string{"Yes", "Yes<U+200B>", "No", "<U+0020>No<U+0020>", "Maybe", "<U+001B>[1mMaybe<U+001B>[0m"}
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("DistinctLines[%d] = %q, want %q", i, got[i], want[i])
		}
	}
	if apart := DistinctLines([]string{"One", "Two words"}); apart[1] != "Two words" {
		t.Errorf("DistinctLines of distinct labels = %q, want plain projections", apart)
	}
	if same := DistinctLines([]string{"Same", "Same"}); same[0] != "Same" || same[1] != "Same" {
		t.Errorf("DistinctLines of equal sources = %q, want plain projections", same)
	}
}
