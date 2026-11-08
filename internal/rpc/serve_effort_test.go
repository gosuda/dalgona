package rpc

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func effortTurn(t *testing.T, c *wireClient, threadID, effort, text string) {
	t.Helper()
	params := map[string]any{"threadId": threadID, "input": []InputItem{{Type: "text", Text: text}}}
	if effort != "" {
		params["effort"] = effort
	}
	var response TurnStartResponse
	c.ok("turn/start", params, &response)
	expect(t, response.Turn.ID != "" && response.Turn.Status == statusInProgress, "turn/start", response.Turn)
	log := readTurn(t, c.collect("turn/completed"))
	expect(t, log.status == statusCompleted, "turn status", log.status)
}

func TestThreadStartConfigEffortDefaultsTurns(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	requests := recordRequests(t, r)
	r.llm.push(llmReply{body: sseText("one")}, llmReply{body: sseText("two")}, llmReply{body: sseText("three")})
	c := r.connect()
	var started ThreadStartResponse
	c.ok("thread/start", map[string]any{
		"cwd": r.cwd, "model": "gpt-6.1-sol",
		"config": map[string]any{"model_reasoning_effort": "high"},
	}, &started)
	expect(t, started.ReasoningEffort == "high", "thread/start reasoningEffort", started.ReasoningEffort)
	c.await("thread/started", nil)
	threadID := started.Thread.ID
	effortTurn(t, c, threadID, "", "default effort")
	effortTurn(t, c, threadID, "low", "turn effort")
	seen := requests.seen()
	expect(t, len(seen) == 2 && seen[0].effort == "high" && seen[1].effort == "low", "model request efforts", seen)

	reopen(t, r)
	recovered := r.connect()
	var resumed ThreadStartResponse
	recovered.ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	expect(t, resumed.ReasoningEffort == "high", "thread/resume reasoningEffort", resumed.ReasoningEffort)
	effortTurn(t, recovered, threadID, "", "after resume")
	seen = requests.seen()
	expect(t, len(seen) == 3 && seen[2].effort == "high", "default effort after resume", seen)
}

func TestThreadStartRejectsUnknownConfigEffort(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	c.fails("thread/start", map[string]any{
		"cwd":    r.cwd,
		"config": map[string]any{"model_reasoning_effort": "loud"},
	}, codeInvalidParams, "unsupported model_reasoning_effort loud")
}
