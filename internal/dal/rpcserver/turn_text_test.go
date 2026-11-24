package rpcserver

import (
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func TestTurnInputRefusesOneInvalidItemBeforeAnyEffect(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	missing := filepath.Join(t.TempDir(), "never-read.png")
	cases := map[string]struct {
		input []map[string]string
		want  string
	}{
		"NUL text after an image": {
			[]map[string]string{{"type": "text", "text": "fine"}, {"type": "localImage", "path": missing}, {"type": "text", "text": "a\x00b"}},
			"invalid text: input[2].text: disallowed_control",
		},
		"NUL mention path": {
			[]map[string]string{{"type": "mention", "path": "/tmp/a\x00b"}, {"type": "text", "text": "fine"}},
			"invalid text: input[0].path: disallowed_control",
		},
		"NUL image path": {
			[]map[string]string{{"type": "localImage", "path": missing + "\x00"}},
			"invalid text: input[0].path: disallowed_control",
		},
	}
	for name, tc := range cases {
		for _, method := range []string{"turn/start", "turn/steer"} {
			reply := c.call(method, map[string]any{"threadId": threadID, "expectedTurnId": "none", "input": tc.input})
			if reply.Error == nil || reply.Error.Code != rpc.CodeInvalidParams || reply.Error.Message != tc.want {
				t.Errorf("%s via %s: reply %+v, want -32602 %q", name, method, reply.Error, tc.want)
			}
		}
	}
	if messages := userMessages(t, r, threadID); len(messages) != 0 {
		t.Fatalf("user messages %+v, want no journal append", messages)
	}
}

func TestTurnInputKeepsExactText(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	text := "  e\u0301 \u1100\u1161\u11a8 \ue000\U000F0001 \ufeff\ufffd\r\n\u2028 "
	var response rpc.TurnStartResponse
	c.ok("turn/start", map[string]any{"threadId": threadID, "input": []map[string]string{{"type": "text", "text": text}}}, &response)
	c.collect()
	messages := userMessages(t, r, threadID)
	if len(messages) != 1 || messages[0].Text != text {
		t.Fatalf("user messages %+v, want the exact text %q", messages, text)
	}
}

func TestThreadNameRefusesNULAndKeepsExactText(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	name := "  \u1100\u1161 \ue000\ufeff\u202e  "
	c.ok("thread/name/set", map[string]any{"threadId": threadID, "name": name}, nil)
	c.fails("thread/name/set", map[string]any{"threadId": threadID, "name": "a\x00b"}, rpc.CodeInvalidParams, "invalid text: name: disallowed_control")
	var read rpc.ThreadReadResponse
	c.ok("thread/read", map[string]any{"threadId": threadID}, &read)
	if read.Thread.Name != name {
		t.Fatalf("thread name %q, want the exact accepted name %q", read.Thread.Name, name)
	}
}
