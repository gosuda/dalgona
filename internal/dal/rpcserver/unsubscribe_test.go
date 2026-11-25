package rpcserver

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func TestLegacyUnsubscribeWithdrawsThatWindowsPendingApproval(t *testing.T) {
	r := newRig(t, config.ApprovalAsk)
	r.llm.push(llmReply{body: sseCall("call-exec", "exec", `{"command":"echo hi"}`)})
	first, second, threadID := openApprovalInTwoWindows(t, r)
	startTurn(t, first, threadID, "run it")
	stale := awaitServerRequest(t, first)
	requestID := approvalRequestID(t, stale)
	live := awaitServerRequest(t, second)
	var result struct {
		Status string `json:"status"`
	}
	first.ok("thread/unsubscribe", map[string]string{"threadId": threadID}, &result)
	if result.Status != "unsubscribed" {
		t.Fatalf("unsubscribe = %q, want unsubscribed", result.Status)
	}
	first.respond(stale.ID, `{"decision":"accept"}`)
	second.respond(live.ID, `{"decision":"decline"}`)
	resolved := second.await("serverRequest/resolved", func(m message) bool {
		return fieldOf(t, m.Params, "requestId") == requestID
	})
	if got := fieldOf(t, resolved.Params, "resolution"); got != "declined" {
		t.Fatalf("resolution = %q, want declined from the subscribed window; the unsubscribed window answered", got)
	}
}
