package tui

import (
	"errors"
	"regexp"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/textinput"
)

func TestSanitizeTextStripsControlsAndEscapes(t *testing.T) {
	t.Parallel()
	cases := []struct{ in, want string }{
		{"plain", "plain"},
		{"line\nkeep\ttab", "line\nkeep\ttab"},
		{"\x1b[8mhidden\x1b[0m shown", "hidden shown"},
		{"\x1b]0;title\x07after", "after"},
		{"\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\", "link"},
		{"naïve", "naïve"},
		{"bad\x00null\x1b[2Jok", "badnullok"},
		{"\x1b(Bcharset", "charset"},
		{"trunc\x1b[3", "trunc"},
		{"x\x9b31my\x9d52;c\x07", "x\uFFFD31my\uFFFD52;c"},
	}
	for _, tc := range cases {
		if got := sanitizeText(tc.in); got != tc.want {
			t.Errorf("sanitizeText(%q) = %q, want %q", tc.in, got, tc.want)
		}
	}
}

func TestRenderSettledItemStripsEscapes(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 80
	item := rpc.Item{Type: "commandExecution", Command: "\x1b[8mrm -rf /\x1b[0m", Output: "esc\x1b]0;pwn\x07aped"}
	rendered := settledText(&m, item)
	if strings.ContainsAny(rendered, "\x1b\x07") {
		t.Fatalf("settled command item kept escapes: %q", rendered)
	}
	if !strings.Contains(rendered, "rm -rf /") {
		t.Fatalf("command text lost: %q", rendered)
	}
	agent := settledText(&m, rpc.Item{Type: "agentMessage", Text: "\x1b[8mconcealed\x1b[0m visible"})
	if !strings.Contains(sanitizeText(agent), "concealed visible") {
		t.Fatalf("agent message text lost after sanitize: %q", agent)
	}
}

func TestRenderApprovalShowsEveryEscape(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 80
	m.screen.theme = newTheme(themeNight, false)
	m.approvals = []approval{{threadID: "t1", command: "\x1b[8msecret\x1b[0m echo hi", reason: "why\x1b]0;t\x07not"}}
	view := strings.Join(framePanel(&m.screen, m.approvalPanel(), panelRows(panelApproval, 24)), "\n")
	plain := regexp.MustCompile("\x1b\\[[0-9;]*m").ReplaceAllString(view, "")
	if strings.ContainsAny(plain, "\x1b\x07") {
		t.Fatalf("approval view kept escapes: %q", view)
	}
	for _, want := range []string{"│   $ <U+001B>[8msecret<U+001B>[0m echo hi", "│   reason: why<U+001B>]0;t<U+0007>not"} {
		found := false
		for line := range strings.SplitSeq(plain, "\n") {
			if strings.TrimRight(line, " ") == want {
				found = true
			}
		}
		if !found {
			t.Fatalf("approval view lacks visible line %q: %q", want, view)
		}
	}
}

func TestDisplayTextEscapesControls(t *testing.T) {
	t.Parallel()
	cases := []struct{ in, want string }{
		{"plain", "plain"},
		{"line\nkeep\ttab", "line\nkeep\ttab"},
		{"a\x01b\x7fc", "a<U+0001>b<U+007F>c"},
		{"x\u0080y", "x<U+0080>y"},
		{"\x1b[8mhidden\x1b[0m shown", "<U+001B>[8mhidden<U+001B>[0m shown"},
		{"x\x1b]0;pwn\x07y", "x<U+001B>]0;pwn<U+0007>y"},
		{"lone\x1b", "lone<U+001B>"},
		{"naïve", "naïve"},
		{"é", "é"},
		{"한", "한"},
		{"a\u200eb", "a<U+200E>b"},
		{"a b", "a b"},
		{"a\U000F0001b", "a\U000F0001b"},
		{"a￾b", "a￾b"},
		{"👨\u200d👩", "👨\u200d👩"},
		{"1️⃣", "1️⃣"},
	}
	for _, tc := range cases {
		if got := displayText(tc.in); got != tc.want {
			t.Errorf("displayText(%q) = %q, want %q", tc.in, got, tc.want)
		}
	}
}

func TestDisplayTextIsIdempotent(t *testing.T) {
	t.Parallel()
	for _, text := range []string{"a\x01b", "x\x1b[31mred\x1b[0m", "plain", "a\U000F0001b"} {
		once := displayText(text)
		if twice := displayText(once); twice != once {
			t.Errorf("displayText(%q) applied twice = %q, want %q", text, twice, once)
		}
	}
}

func TestContainsNUL(t *testing.T) {
	t.Parallel()
	if !errors.Is(fieldPolicy.Check("a\x00b"), textinput.DisallowedControl) {
		t.Error("field policy missed an embedded NUL")
	}
	if err := fieldPolicy.Check("plain"); err != nil {
		t.Errorf("field policy flagged clean text: %v", err)
	}
}
