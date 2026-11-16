package rpc

import (
	"testing"
	"time"
)

func TestServeApprovalShowsExactCommand(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	const exact = `echo a; echo b`
	r.llm.push(
		llmReply{body: sseCall("call-exec", "exec", `{"command":"`+exact+`"}`)},
		llmReply{body: sseText("done")},
	)
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "run it")
	var req message
	select {
	case req = <-c.requests:
	case <-time.After(waitLimit):
		t.Fatal("no approval request")
	}
	if got := fieldOf(t, req.Params, "command"); got != exact {
		t.Fatalf("approval command %q, want exact %q", got, exact)
	}
	c.respond(req.ID, `{"decision":"accept"}`)
	log := readTurn(t, c.collect())
	expect(t, log.status == statusCompleted, "turn status", log.status)
}
