package rpc

import (
	json "encoding/json/v2"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/config"
)

func awaitServerRequest(t *testing.T, c *wireClient) message {
	t.Helper()
	select {
	case request := <-c.requests:
		return request
	case <-time.After(waitLimit):
		t.Fatal("no approval request")
		return message{}
	}
}

func approvalRequestID(t *testing.T, request message) string {
	t.Helper()
	var id string
	if err := json.Unmarshal(request.ID, &id); err != nil {
		t.Fatal(err)
	}
	return id
}

func openApprovalInTwoWindows(t *testing.T, r *rig) (first, second *wireClient, threadID string) {
	t.Helper()
	first = r.connect()
	threadID = startThread(t, r, first, nil)
	second = r.connect()
	var resumed ThreadResumeResponse
	second.ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	return first, second, threadID
}

func TestApprovalResolutionReachesOtherWindow(t *testing.T) {
	cases := []struct {
		name, answer, resolution string
	}{
		{name: "accept", answer: `{"decision":"accept"}`, resolution: "answered"},
		{name: "decline", answer: `{"decision":"decline"}`, resolution: "declined"},
		{name: "cancel", answer: `{"decision":"cancel"}`, resolution: "cancelled"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			r := newRig(t, config.ApprovalAsk)
			r.llm.push(llmReply{body: sseCall("call-exec", "exec", `{"command":"echo hi"}`)})
			first, second, threadID := openApprovalInTwoWindows(t, r)
			startTurn(t, first, threadID, "run it")
			request := awaitServerRequest(t, first)
			requestID := approvalRequestID(t, request)
			_ = awaitServerRequest(t, second)
			first.respond(request.ID, tc.answer)
			resolved := second.await("serverRequest/resolved", func(m message) bool {
				return fieldOf(t, m.Params, "requestId") == requestID
			})
			if got := fieldOf(t, resolved.Params, "resolution"); got != tc.resolution {
				t.Fatalf("resolution = %q, want %q; params %s", got, tc.resolution, resolved.Params)
			}
		})
	}
}

func TestApprovalInterruptResolutionReachesOtherWindow(t *testing.T) {
	r := newRig(t, config.ApprovalAsk)
	r.llm.push(llmReply{body: sseCall("call-exec", "exec", `{"command":"echo hi"}`)})
	first, second, threadID := openApprovalInTwoWindows(t, r)
	turnID := startTurn(t, first, threadID, "run it")
	request := awaitServerRequest(t, first)
	requestID := approvalRequestID(t, request)
	_ = awaitServerRequest(t, second)
	first.ok("turn/interrupt", map[string]any{"threadId": threadID, "turnId": turnID}, &struct{}{})
	resolved := second.await("serverRequest/resolved", func(m message) bool {
		return fieldOf(t, m.Params, "requestId") == requestID
	})
	if got := fieldOf(t, resolved.Params, "resolution"); got != "interrupted" {
		t.Fatalf("resolution = %q, want interrupted; params %s", got, resolved.Params)
	}
}
