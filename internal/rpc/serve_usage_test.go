package rpc

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestTokenUsageCarriesContextWindowAndSurvivesResume(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	r.cfg.Config.Models = map[string]config.ModelSpec{"openai/gpt-test": {ContextWindow: 200000}}
	reopen(t, r)
	r.llm.push(llmReply{body: sseText("hi")})
	first := r.connect()
	threadID := startThread(t, r, first, nil)
	startTurn(t, first, threadID, "hello")

	var live struct {
		TokenUsage struct {
			ModelContextWindow int `json:"modelContextWindow"`
		} `json:"tokenUsage"`
	}
	mustUnmarshal(t, first.await("thread/tokenUsage/updated", nil).Params, &live)
	if live.TokenUsage.ModelContextWindow != 200000 {
		t.Fatalf("live modelContextWindow = %d, want 200000", live.TokenUsage.ModelContextWindow)
	}
	first.await("turn/completed", nil)

	second := r.connect()
	var resumed struct {
		TokenUsage *struct {
			Total struct {
				TotalTokens int `json:"totalTokens"`
			} `json:"total"`
			ModelContextWindow int `json:"modelContextWindow"`
		} `json:"tokenUsage"`
	}
	second.ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	if resumed.TokenUsage == nil {
		t.Fatal("thread/resume has no tokenUsage member")
	}
	if resumed.TokenUsage.Total.TotalTokens != 27 {
		t.Fatalf("resumed total tokens = %d, want 27", resumed.TokenUsage.Total.TotalTokens)
	}
	if resumed.TokenUsage.ModelContextWindow != 200000 {
		t.Fatalf("resumed modelContextWindow = %d, want 200000", resumed.TokenUsage.ModelContextWindow)
	}
}
