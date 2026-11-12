package tui

import (
	"regexp"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/rpc"
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

func TestRenderApprovalStripsEscapes(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 80
	m.approvals = []approval{{threadID: "t1", command: "\x1b[8msecret\x1b[0m echo hi", reason: "why\x1b]0;t\x07not"}}
	view := strings.Join(framePanel(&m.screen, m.approvalPanel(), panelRows(panelApproval, 24)), "\n")
	plain := regexp.MustCompile("\x1b\\[[0-9;]*m").ReplaceAllString(view, "")
	if strings.ContainsAny(plain, "\x1b\x07") {
		t.Fatalf("approval view kept escapes: %q", view)
	}
	if !strings.Contains(view, "echo hi") {
		t.Fatalf("approval command lost: %q", view)
	}
}
