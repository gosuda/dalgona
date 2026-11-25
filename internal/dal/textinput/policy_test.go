package textinput

import (
	"errors"
	"strings"
	"testing"
)

var (
	singleLine = Policy{Lines: SingleLine, Limit: Limit{Unit: Bytes, Max: 1 << 16}}
	multiLine  = Policy{Lines: MultiLine, Limit: Limit{Unit: Bytes, Max: 1 << 16}}
	lineBreaks = []string{"\n", "\r", "\v", "\f", "\u0085", "\u2028", "\u2029", "\r\n"}
)

type checkCase struct {
	name   string
	policy Policy
	text   string
	want   error
}

func TestCheckRejectsWithSharedReasons(t *testing.T) {
	cases := []checkCase{
		{"malformed utf-8", multiLine, "ok\xc0\xaf", InvalidEncoding},
		{"utf-8 encoded surrogate", multiLine, "\xed\xa0\x80", InvalidEncoding},
		{"nul in multiline", multiLine, "a\x00b", DisallowedControl},
		{"nul in single line", singleLine, "\x00", DisallowedControl},
		{"literal replacement character", singleLine, "\ufffd", nil},
		{"escape and bidi controls stay data", singleLine, "\x1b[2J\u202e", nil},
		{"multiline keeps crlf of an offered or direct value", Answer, "Yes\r\nwith CRLF\r", nil},
		{"answer at its scalar quota", Answer, strings.Repeat("\U0001F600", 2000), nil},
		{"answer over its scalar quota", Answer, strings.Repeat("a", 2001), TextTooLarge},
		{"byte quota counts bytes", Policy{Lines: MultiLine, Limit: Limit{Unit: Bytes, Max: 3}}, "\u00e9\u00e9", TextTooLarge},
	}
	for _, breakText := range lineBreaks {
		cases = append(cases,
			checkCase{"single line rejects " + breakText, singleLine, "a" + breakText + "b", LineBreakNotAllowed},
			checkCase{"multiline keeps " + breakText, multiLine, "a" + breakText + "b", nil},
		)
	}
	for _, c := range cases {
		if err := c.policy.Check(c.text); !errors.Is(err, c.want) {
			t.Errorf("%q: Check = %v, want %v", c.name, err, c.want)
		}
	}
}

func TestAnswerQuotaCountsScalarsNotUnitsOrClusters(t *testing.T) {
	emoji := strings.Repeat("\U0001F600", 2000)
	if Count(emoji, UTF16Units) != 4000 || Count(emoji, Bytes) != 8000 {
		t.Fatalf("fixture counts %d UTF-16 units and %d bytes", Count(emoji, UTF16Units), Count(emoji, Bytes))
	}
	if err := Answer.Check(emoji); err != nil {
		t.Fatalf("2000 supplementary scalars: %v", err)
	}
	cluster := "e" + strings.Repeat("\u0301", 2000)
	if got := len(Boundaries(cluster)); got != 2 {
		t.Fatalf("fixture is %d boundaries, want one cluster", got)
	}
	if err := Answer.Check(cluster); !errors.Is(err, TextTooLarge) {
		t.Fatalf("one cluster of 2001 scalars: %v, want %v", err, TextTooLarge)
	}
}

func TestCheckRequestRejectsTheWholeRequest(t *testing.T) {
	secret := "token\x00value"
	items := []Item{
		{Field: "input[0].text", Policy: multiLine, Text: "first"},
		{Field: "input[1].text", Policy: multiLine, Text: secret},
		{Field: "input[2].text", Policy: singleLine, Text: "a\nb"},
	}
	err := CheckRequest(items)
	var fieldErr *FieldError
	if !errors.As(err, &fieldErr) || fieldErr.Field != "input[1].text" || !errors.Is(err, DisallowedControl) {
		t.Fatalf("CheckRequest = %v, want the first invalid item input[1].text", err)
	}
	if got, want := err.Error(), "invalid text: input[1].text: disallowed_control"; got != want {
		t.Fatalf("message %q, want %q", got, want)
	}
	if strings.Contains(err.Error(), "token") {
		t.Fatalf("message %q echoes the submitted value", err)
	}
	if err := CheckRequest(items[:1]); err != nil {
		t.Fatalf("valid request: %v", err)
	}
}

func TestReplaceRejectsMultilineInSingleLineFieldWhole(t *testing.T) {
	before := Draft{Text: "keep me", Selection: Range{Start: 5, End: 7}}
	for _, breakText := range lineBreaks {
		got, err := before.Replace(singleLine, "first"+breakText+"second")
		if !errors.Is(err, LineBreakNotAllowed) || got != before {
			t.Errorf("break %q: Replace = %+v, %v; want the previous draft and %v", breakText, got, err, LineBreakNotAllowed)
		}
	}
}

func TestReplaceConvertsNewlinesOnceOnNewText(t *testing.T) {
	draft := Draft{Text: "old\r\n", Selection: Range{Start: 5, End: 5}}
	typed, err := draft.Replace(multiLine, "a\r\nb\rc\vd\fe\u0085f\u2028g\u2029h\n")
	if err != nil {
		t.Fatalf("Replace: %v", err)
	}
	want := "old\r\na\nb\nc\vd\fe\u0085f\u2028g\u2029h\n"
	if typed.Text != want || typed.Selection != (Range{Start: len(want), End: len(want)}) {
		t.Fatalf("Replace = %q at %v, want %q with the caret at the end", typed.Text, typed.Selection, want)
	}
	saved := typed.Text[len(draft.Text):]
	restored, err := Draft{}.Replace(multiLine, saved)
	if err != nil || restored.Text != saved {
		t.Fatalf("restore = %q, %v; want %q", restored.Text, err, saved)
	}
}

func TestReplaceQuotaIsAtomic(t *testing.T) {
	draft := Draft{Text: strings.Repeat("a", 1999), Selection: Range{Start: 1999, End: 1999}}
	cases := []struct {
		name      string
		candidate string
		want      error
	}{
		{"one scalar fits", "\U0001F600", nil},
		{"two scalars exceed", "bc", TextTooLarge},
		{"one cluster of 2001 scalars", "e" + strings.Repeat("\u0301", 2000), TextTooLarge},
		{"one cluster of 100001 scalars", "e" + strings.Repeat("\u0301", 100000), TextTooLarge},
		{"malformed candidate", "\xe2\x82", InvalidEncoding},
	}
	for _, c := range cases {
		got, err := draft.Replace(Answer, c.candidate)
		if c.want == nil {
			if err != nil || Count(got.Text, Scalars) != 2000 {
				t.Errorf("%s: Replace = %d scalars, %v", c.name, Count(got.Text, Scalars), err)
			}
			continue
		}
		if !errors.Is(err, c.want) || got != draft {
			t.Errorf("%s: Replace error %v, changed %t; want %v and the previous draft", c.name, err, got != draft, c.want)
		}
	}
}

func TestReplaceSelectionAndCaretFollowClusters(t *testing.T) {
	cases := []struct {
		name      string
		draft     Draft
		candidate string
		want      Draft
	}{
		{"replace a selection", Draft{Text: "abc", Selection: Range{Start: 1, End: 2}}, "XY", Draft{Text: "aXYc", Selection: Range{Start: 3, End: 3}}},
		{"base letter joins a following mark", Draft{Text: "\u0301x", Selection: Range{}}, "e", Draft{Text: "e\u0301x", Selection: Range{Start: 3, End: 3}}},
		{"regional indicator repairs pairing", Draft{Text: "\U0001F1F0\U0001F1F7", Selection: Range{}}, "\U0001F1FA", Draft{Text: "\U0001F1FA\U0001F1F0\U0001F1F7", Selection: Range{Start: 8, End: 8}}},
		{"joiner continues an emoji", Draft{Text: "\U0001F468\U0001F469", Selection: Range{Start: 4, End: 4}}, "\u200d", Draft{Text: "\U0001F468\u200d\U0001F469", Selection: Range{Start: 11, End: 11}}},
		{"private use stays text", Draft{Text: "a", Selection: Range{Start: 1, End: 1}}, "\ue000\U000F0001", Draft{Text: "a\ue000\U000F0001", Selection: Range{Start: 8, End: 8}}},
	}
	for _, c := range cases {
		got, err := c.draft.Replace(multiLine, c.candidate)
		if err != nil || got != c.want {
			t.Errorf("%s: Replace = %#v, %v; want %#v", c.name, got, err, c.want)
		}
	}
}

func TestReplaceRejectsSelectionsOffClusterBoundaries(t *testing.T) {
	text := "e\u0301\U0001F1F0\U0001F1F7"
	for _, r := range []Range{{Start: 1, End: 1}, {Start: 0, End: 2}, {Start: 3, End: 7}, {Start: 3, End: 4}, {Start: 3, End: 0}, {Start: -1, End: 0}, {Start: 0, End: 12}} {
		draft := Draft{Text: text, Selection: r}
		got, err := draft.Replace(multiLine, "x")
		if !errors.Is(err, InvalidOffset) || got != draft {
			t.Errorf("selection %v: Replace = %#v, %v; want %v", r, got, err, InvalidOffset)
		}
	}
}
