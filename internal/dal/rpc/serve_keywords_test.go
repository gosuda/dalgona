package rpc

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
)

type wireKeywordMatch struct {
	Word      string `json:"word"`
	ByteRange struct {
		Start int `json:"start"`
		End   int `json:"end"`
	} `json:"byteRange"`
	Effort  string `json:"effort"`
	Summary string `json:"summary"`
}

func TestKeywordMatchReportsRangeAndEffect(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()

	var prompt struct {
		Matches []wireKeywordMatch `json:"matches"`
	}
	c.ok("dalgona/keywords/match", map[string]any{"text": "please ultrathink, not `ultrathink` ", "kind": "prompt"}, &prompt)
	expect(t, len(prompt.Matches) == 1, "prompt matches", prompt)
	got := prompt.Matches[0]
	expect(t, got.Word == "ultrathink" && got.ByteRange.Start == 7 && got.ByteRange.End == 17, "prompt range", got)
	expect(t, got.Effort == "max" && got.Summary == "effort max, this turn", "prompt effect", got)

	var steer struct {
		Matches []wireKeywordMatch `json:"matches"`
	}
	c.ok("dalgona/keywords/match", map[string]any{"text": "ultrathink", "kind": "steer"}, &steer)
	expect(t, len(steer.Matches) == 1 && steer.Matches[0].Summary == "notice added" && steer.Matches[0].Effort == "", "steer effect", steer.Matches)

	r.llm.push(llmReply{body: sseText("done")})
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "ultrathink now")
	log := readTurn(t, c.collect())
	user := log.completed[0]
	expect(t, len(user.Keywords) == 1 && user.Keywords[0].ByteRange.Start == 0 && user.Keywords[0].ByteRange.End == 10, "userMessage keywords", user.Keywords)
}

func TestKeywordsMatchRefusesTextOver64KiB(t *testing.T) {
	cases := []struct {
		name    string
		text    string
		refused bool
	}{
		{"65536 bytes are not over 64 KiB", strings.Repeat("a", 65536), false},
		{"65537 bytes are over 64 KiB", strings.Repeat("a", 65537), true},
		{"21846 three-byte runes are 65538 bytes and over 64 KiB", strings.Repeat("\u20ac", 21846), true},
		{"21845 three-byte runes are 65535 bytes and not over 64 KiB", strings.Repeat("\u20ac", 21845), false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			c := newRig(t, config.ApprovalYolo).connect()
			params := map[string]any{"text": tc.text}
			if tc.refused {
				c.fails("dalgona/keywords/match", params, -32602, "text exceeds 64 KiB")
				return
			}
			c.ok("dalgona/keywords/match", params, nil)
		})
	}
}
