package rpc

import (
	"encoding/json/jsontext"
	"testing"
	"time"
)

func awaitFirstApproval(t *testing.T, c *wireClient) message {
	t.Helper()
	select {
	case first := <-c.requests:
		return first
	case <-time.After(waitLimit):
		t.Fatal("no first child approval request")
		return message{}
	}
}

func awaitItemApproval(t *testing.T, c *wireClient, itemID string) message {
	t.Helper()
	deadline := time.After(waitLimit)
	for {
		select {
		case m := <-c.requests:
			if fieldOf(t, m.Params, "itemId") == itemID {
				return m
			}
		case <-deadline:
			t.Fatal("no second child approval request")
		}
	}
}

func countDuplicateDeliveries(t *testing.T, c *wireClient, id jsontext.Value, quiet time.Duration) int {
	t.Helper()
	duplicates := 0
	end := time.Now().Add(quiet)
	for time.Now().Before(end) {
		select {
		case m := <-c.requests:
			if string(m.ID) == string(id) {
				duplicates++
			}
		case <-time.After(50 * time.Millisecond):
		}
	}
	return duplicates
}

func TestServeChildApprovalSentOncePerConnection(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	r.llm.push(
		llmReply{body: sseCall("call-task", "task", `{"prompt":"run it"}`)},
		llmReply{body: sseCall("call-first", "exec", `{"command":"echo first"}`)},
		llmReply{body: sseCall("call-second", "exec", `{"command":"echo second"}`)},
		llmReply{body: sseCall("call-yield", "yield", `{"data":"ok"}`)},
		llmReply{body: sseText("parent done")},
	)
	parentID := startThread(t, r, c, nil)
	startTurn(t, c, parentID, "delegate")
	first := awaitFirstApproval(t, c)
	childID := fieldOf(t, first.Params, "threadId")
	if childID == "" || childID == parentID {
		t.Fatalf("child thread id %q", childID)
	}
	var resumed ThreadResumeResponse
	c.ok("thread/resume", map[string]any{"threadId": childID}, &resumed)
	c.respond(first.ID, `{"decision":"accept"}`)
	second := awaitItemApproval(t, c, "call-second")
	if duplicates := countDuplicateDeliveries(t, c, second.ID, 2*time.Second); duplicates != 0 {
		t.Fatalf("duplicate approval request %s delivered %d extra times", second.ID, duplicates)
	}
	c.respond(second.ID, `{"decision":"accept"}`)
	log := readTurn(t, c.collect("turn/completed"))
	expect(t, log.status == statusCompleted, "parent turn status", log.status)
}
