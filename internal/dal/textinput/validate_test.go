package textinput

import (
	"strings"
	"testing"
)

func TestAnswerQuotaCountsScalarsAndRejectsWhole(t *testing.T) {
	answer := Field{Name: "answers.q1", MaxScalars: AnswerScalars}
	emoji := strings.Repeat("\U0001F600", AnswerScalars)
	cases := []struct {
		name string
		text string
		want Reason
	}{
		{"2000 scalars", strings.Repeat("a", AnswerScalars), ""},
		{"2001 scalars", strings.Repeat("a", AnswerScalars+1), TextTooLarge},
		{"2000 supplementary emoji, 4000 UTF-16 units", emoji, ""},
		{"one cluster of 2001 scalars", "a" + strings.Repeat("\u0301", AnswerScalars), TextTooLarge},
		{"one cluster of 100001 scalars", "a" + strings.Repeat("\u0301", 100000), TextTooLarge},
	}
	for _, c := range cases {
		err := answer.Check(c.text)
		if c.want == "" && err != nil || c.want != "" && !isReason(err, c.want) {
			t.Errorf("%s: err = %v, want %q", c.name, err, c.want)
		}
	}
}

func TestSingleLineFieldsRefuseEveryLineBreak(t *testing.T) {
	title := Field{Name: "name", SingleLine: true}
	for _, br := range []string{"\n", "\r", "\v", "\f", "\u0085", "\u2028", "\u2029"} {
		if err := title.Check("a" + br + "b"); !isReason(err, LineBreakNotAllowed) {
			t.Errorf("%q: err = %v, want line_break_not_allowed", br, err)
		}
	}
	if err := title.Check("a\tb\u200d"); err != nil {
		t.Fatalf("tab and joiner: %v", err)
	}
}

func TestErrorNamesFieldAndReasonWithoutTheText(t *testing.T) {
	err := Field{Name: "input[2].text"}.Check("secret\x00value")
	if err == nil || err.Error() != "invalid text: input[2].text: disallowed_control" {
		t.Fatalf("err = %v", err)
	}
}

func TestBlankAndSearchEligibilityUseWhiteSpaceWithoutChangingText(t *testing.T) {
	if !Blank(" \u3000\u2028\t") || Blank(" \u200b ") {
		t.Fatal("Blank must follow White_Space: ideographic space is blank, zero-width space is not")
	}
	cases := map[string]bool{
		"":       false,
		"   ":    false,
		"abc":    true,
		"abc de": false,
		"\ud55c\uad6d\uc5b4\u3000\uac80\uc0c9\uc5b4": true,
		"ab ":              false,
		"\ud55c\uad6d abc": false,
		"  abc  ":          true,
	}
	for text, want := range cases {
		if got := SearchEligible(text); got != want {
			t.Errorf("SearchEligible(%q) = %v, want %v", text, got, want)
		}
	}
}

func TestNewlinesToLFConvertsOnce(t *testing.T) {
	if got := NewlinesToLF("a\r\nb\rc\n\u2028"); got != "a\nb\nc\n\u2028" {
		t.Fatalf("got %q", got)
	}
}
