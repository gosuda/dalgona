package rpc

import (
	json "encoding/json/v2"
	"net/http"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

const rateLimitBody = `{"error":{"type":"rate_limit_error"}}`

type retryWaitParams struct {
	ThreadID    string `json:"threadId"`
	TurnID      string `json:"turnId"`
	RemainingMS int64  `json:"remainingMs"`
}

func TestRetryWaitNotification(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	r.llm.push(llmReply{status: http.StatusTooManyRequests, retryAfter: "90", body: rateLimitBody}, llmReply{body: sseText("done")})
	c := r.connect()
	var started ThreadStartResponse
	c.ok("thread/start", map[string]any{"cwd": r.cwd}, &started)
	c.await("thread/started", nil)
	var turn TurnStartResponse
	c.ok("turn/start", map[string]any{"threadId": started.Thread.ID, "input": []InputItem{{Type: "text", Text: "wait"}}}, &turn)
	var waits []retryWaitParams
	for _, m := range c.collect("turn/completed") {
		if m.Method != "dalgona/retry/wait" {
			continue
		}
		var params retryWaitParams
		if err := json.Unmarshal(m.Params, &params); err != nil {
			t.Fatal(err)
		}
		waits = append(waits, params)
	}
	if len(waits) < 2 {
		t.Fatalf("retry wait notifications = %+v, want a countdown", waits)
	}
	first, last := waits[0], waits[len(waits)-1]
	expect(t, first.RemainingMS == 90000, "first remainingMs", first.RemainingMS)
	expect(t, last.RemainingMS == 0, "last remainingMs", last.RemainingMS)
	expect(t, first.ThreadID == started.Thread.ID && first.TurnID == turn.Turn.ID, "thread and turn ids", first)
}
