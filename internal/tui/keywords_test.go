package tui

import (
	"context"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/rpc"
)

type failingKeywordCaller struct {
	fakeCaller
}

func (f *failingKeywordCaller) Call(ctx context.Context, method string, params, result any) error {
	if method == "dalgona/keywords/match" {
		return &rpc.RPCError{Code: -32601, Message: "method not found"}
	}
	return f.fakeCaller.Call(ctx, method, params, result)
}

func keywordReplyMsg(seq int, text string, ranges ...[2]int) resultMsg {
	reply := new(keywordReply)
	for _, r := range ranges {
		reply.Matches = append(reply.Matches, keywordReplyMatch{ByteRange: keywordRange{Start: r[0], End: r[1]}})
	}
	return resultMsg{op: "keywordMatch", value: reply, seq: seq, text: text}
}

func underlinedRunes(text string) string {
	var out strings.Builder
	under, last := false, 0
	for _, loc := range sgrPattern.FindAllStringSubmatchIndex(text, -1) {
		if under {
			out.WriteString(text[last:loc[0]])
		}
		under = underlineAfter(under, text[loc[2]:loc[3]])
		last = loc[1]
	}
	if under {
		out.WriteString(text[last:])
	}
	return out.String()
}

func underlineAfter(under bool, params string) bool {
	for param := range strings.SplitSeq(params, ";") {
		switch param {
		case "4":
			under = true
		case "", "0", "24":
			under = false
		}
	}
	return under
}

func TestKeywordMarkUnderlinedUnderNoColor(t *testing.T) {
	t.Parallel()
	text := "use ultrathink now"
	spans := []keywordSpan{{start: 4, end: 14}}
	for _, tc := range []struct {
		name  string
		theme theme
		open  string
	}{
		{name: "before the theme is known", theme: theme{}, open: "\x1b[1m"},
		{name: "known theme without colour", theme: newTheme(themeNight, false), open: "\x1b[1;4m"},
	} {
		out := markKeywords(text, spans, tc.theme)
		if segment := tc.open + "ultrathink" + reset; !strings.Contains(out, segment) {
			t.Fatalf("%s: mark is %q, want %q", tc.name, out, segment)
		}
		if got := stripANSI(out); got != text {
			t.Fatalf("mark changed visible text to %q", got)
		}
	}
}

func TestComposerAndSentMessageKeywordGlow(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m.screen.theme = newTheme(themeNight, false)
	m.screen.width = 8
	m.input.composer.SetWidth(8)
	m.input.composer.SetValue("ultrathink")
	m.input.marks = []keywordSpan{{start: 0, end: 10}}
	m.input.marksFor = "ultrathink"
	view := m.View().Content
	if got := strings.ReplaceAll(underlinedRunes(view), "\n", ""); got != "ultrathink" {
		t.Fatalf("wrapped composer lost its keyword mark: %q\n%q", got, view)
	}
	m.screen.width = 40
	settled := settledText(&m, rpc.Item{
		Type:     "userMessage",
		Text:     "ultrathink, please",
		Keywords: []rpc.KeywordMatch{{ByteRange: rpc.ByteRange{Start: 0, End: 10}}},
	})
	if got := underlinedRunes(settled); got != "ultrathink" {
		t.Fatalf("settled user message lost its keyword mark: %q", settled)
	}
}

func TestComposerMarksComeFromServerReply(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m.screen.theme = newTheme(themeNight, false)
	m.input.composer.SetValue("please ultrathink")
	if got := underlinedRunes(m.View().Content); got != "" {
		t.Fatalf("composer marked %q before any reply", got)
	}
	_ = m.matchKeywords()
	m, _ = drive(t, m, keywordReplyMsg(m.matchSeq, "please ultrathink", [2]int{7, 17}))
	if got := underlinedRunes(m.View().Content); got != "ultrathink" {
		t.Fatalf("server reply marked %q, want ultrathink", got)
	}
}

func TestStaleKeywordReplyIsDropped(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m.screen.theme = newTheme(themeNight, false)
	m.input.composer.SetValue("x ultrathink")
	_ = m.matchKeywords()
	first := m.matchSeq
	m.input.composer.SetValue("ultrathink")
	_ = m.matchKeywords()
	second := m.matchSeq
	m, _ = drive(t, m, keywordReplyMsg(second, "ultrathink", [2]int{0, 10}))
	m, _ = drive(t, m, keywordReplyMsg(first, "x ultrathink", [2]int{2, 12}))
	if got := underlinedRunes(m.View().Content); got != "ultrathink" {
		t.Fatalf("stale reply changed the marks: %q", got)
	}

	fresh := newTestModel(nil, Options{}, nil)
	fresh.input.composer.SetValue("ultrathink")
	_ = fresh.matchKeywords()
	fresh, _ = drive(t, fresh, keywordReplyMsg(first, "x ultrathink", [2]int{2, 12}))
	if got := underlinedRunes(fresh.View().Content); got != "" {
		t.Fatalf("stale reply marked %q on a new draft", got)
	}
}

func TestKeywordMatchErrorIsSilent(t *testing.T) {
	t.Parallel()
	m := newTestModel(&failingKeywordCaller{}, Options{}, nil)
	m.input.composer.SetValue("ultrathink")
	command := m.matchKeywords()
	m, _ = drive(t, m, command())
	if m.notice != "" || m.noticeError {
		t.Fatalf("a server without keyword matching raised a notice: %q", m.notice)
	}
}

func TestComposerMarkWithCursorInsideWord(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m.screen.theme = newTheme(themeNight, false)
	m.input.composer.SetValue("ultrathink")
	m.input.composer.cursor = 5
	m.input.marks = []keywordSpan{{start: 0, end: 10}}
	m.input.marksFor = "ultrathink"
	view := m.View().Content
	if got := stripANSI(view); !strings.Contains(got, "› ultra▏think") {
		t.Fatalf("cursor inside the word broke the composer: %q", got)
	}
	if got := underlinedRunes(view); got != "ultrathink" {
		t.Fatalf("mark does not cover both parts around the cursor: %q", got)
	}
}

func stripANSI(text string) string {
	var out strings.Builder
	for i := 0; i < len(text); {
		if text[i] != '\x1b' {
			out.WriteByte(text[i])
			i++
			continue
		}
		i += 2
		for i < len(text) && text[i] != 'm' {
			i++
		}
		if i < len(text) {
			i++
		}
	}
	return out.String()
}
