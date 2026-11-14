package rpc

import (
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/config"
)

func awaitApprovalRequest(t *testing.T, c *wireClient) message {
	t.Helper()
	select {
	case m := <-c.requests:
		return m
	case <-time.After(waitLimit):
		t.Fatal("no approval request")
		return message{}
	}
}

func TestApprovalErrorWaitsForOtherWindow(t *testing.T) {
	r := newRig(t, config.ApprovalAsk)
	first := r.connect()
	second := r.connect()
	r.llm.push(
		llmReply{body: sseCall("call-exec", "exec", `{"command":"echo held"}`)},
		llmReply{body: sseText("done")},
	)
	threadID := startThread(t, r, first, nil)
	var resumed ThreadResumeResponse
	second.ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	startTurn(t, first, threadID, "run it")
	firstRequest := awaitApprovalRequest(t, first)
	secondRequest := awaitApprovalRequest(t, second)
	first.send(message{ID: firstRequest.ID, Error: &RPCError{Code: -32603, Message: "window cannot answer"}})
	first.call("thread/read", map[string]any{"threadId": threadID})
	second.respond(secondRequest.ID, `{"decision":"accept"}`)
	log := readTurn(t, first.collect())
	expect(t, log.status == statusCompleted, "turn status", log.status)
	item := findItem(log.completed, "call-exec")
	expect(t, item.Status == statusCompleted && strings.Contains(item.Output, "held"),
		"exec item after the other window accepted", item)
}

func TestApprovalErrorDeclinesWhenAlone(t *testing.T) {
	r := newRig(t, config.ApprovalAsk)
	c := r.connect()
	r.llm.push(
		llmReply{body: sseCall("call-exec", "exec", `{"command":"echo held"}`)},
		llmReply{body: sseText("done")},
	)
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "run it")
	request := awaitApprovalRequest(t, c)
	c.send(message{ID: request.ID, Error: &RPCError{Code: -32603, Message: "window cannot answer"}})
	log := readTurn(t, c.collect())
	item := findItem(log.completed, "call-exec")
	expect(t, item.Status == statusDeclined && !strings.Contains(item.Output, "held"),
		"exec item declined when the only window errors", item)
}

func TestApprovalErrorFromEveryWindowDeclines(t *testing.T) {
	r := newRig(t, config.ApprovalAsk)
	first := r.connect()
	second := r.connect()
	r.llm.push(
		llmReply{body: sseCall("call-exec", "exec", `{"command":"echo held"}`)},
		llmReply{body: sseText("done")},
	)
	threadID := startThread(t, r, first, nil)
	var resumed ThreadResumeResponse
	second.ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	startTurn(t, first, threadID, "run it")
	firstRequest := awaitApprovalRequest(t, first)
	secondRequest := awaitApprovalRequest(t, second)
	first.send(message{ID: firstRequest.ID, Error: &RPCError{Code: -32603, Message: "window cannot answer"}})
	second.send(message{ID: secondRequest.ID, Error: &RPCError{Code: -32603, Message: "window cannot answer"}})
	log := readTurn(t, first.collect())
	item := findItem(log.completed, "call-exec")
	expect(t, item.Status == statusDeclined && !strings.Contains(item.Output, "held"),
		"exec item declined when every attached window errors", item)
}
