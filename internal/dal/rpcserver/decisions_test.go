package rpcserver

import (
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func unsubscribeStatus(t *testing.T, c *nativeClient, threadID string) string {
	t.Helper()
	var result struct {
		Status string `json:"status"`
	}
	c.ok("thread/unsubscribe", map[string]string{"threadId": threadID}, &result)
	return result.Status
}

func TestUnsubscribeDetachesOneConnectionWithoutStoppingTheTurn(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	a := r.connectNative(ServeOptions{})
	b := r.connectNative(ServeOptions{})
	threadID := a.startThread(r)
	var snapA, snapB rpc.HistorySnapshot
	a.ok("dalgona/history/open", map[string]string{"threadId": threadID}, &snapA)
	b.ok("dalgona/history/open", map[string]string{"threadId": threadID}, &snapB)
	gate := make(chan struct{})
	r.llm.push(llmReply{gate: gate, body: sseText("still running")})
	b.ok("turn/start", prompt(threadID, "go"), nil)
	a.awaitEvent("turn/started", nil)
	if status := unsubscribeStatus(t, a, threadID); status != "unsubscribed" {
		t.Fatalf("first unsubscribe = %s", status)
	}
	if status := unsubscribeStatus(t, a, threadID); status != "notSubscribed" {
		t.Fatalf("repeated unsubscribe = %s", status)
	}
	if status := unsubscribeStatus(t, a, "no-such-thread"); status != "notLoaded" {
		t.Fatalf("unknown thread unsubscribe = %s", status)
	}
	_ = a.failsWith("dalgona/history/page", map[string]string{"historyId": snapA.HistoryID}, rpc.ErrorExpired)
	close(gate)
	completed := b.awaitEvent("turn/completed", nil)
	if status := fieldOf(t, completed.Params, "turn", "status"); status != "completed" {
		t.Fatalf("turn after unsubscribe finished %s, want completed", status)
	}
	b.ok("dalgona/history/page", map[string]string{"historyId": snapB.HistoryID}, nil)
	assertNoTurnEvents(t, a)
	var listed rpc.ThreadListResponse
	b.ok("thread/list", map[string]any{}, &listed)
	for _, thread := range listed.Data {
		if thread.ID == threadID {
			return
		}
	}
	t.Fatal("unsubscribe archived the thread")
}

func assertNoTurnEvents(t *testing.T, c *nativeClient) {
	t.Helper()
	settle := time.After(200 * time.Millisecond)
	for {
		select {
		case event := <-c.events:
			if event.Method == "turn/completed" || event.Method == "item/agentMessage/delta" {
				t.Fatalf("unsubscribed connection still received %s", event.Method)
			}
		case <-settle:
			return
		}
	}
}

func TestInvalidAnswerTextKeepsQuestionCorrectable(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connectNative(ServeOptions{})
	args := `{"questions":[{"header":"Database","question":"Which database?","options":[{"label":"SQLite","description":"One file."},{"label":"Postgres","description":"A server."}]}]}`
	r.llm.push(llmReply{body: sseCall("call-q", "ask", args)}, llmReply{body: sseText("done")})
	threadID := c.startThread(r)
	c.ok("turn/start", prompt(threadID, "ask me"), nil)
	request := awaitQuestionRequest(t, c)
	refuseBadAnswer(t, c, threadID, request)
	c.send(message{ID: request.ID, Result: []byte(`{"answers":{"q1":{"answers":["Postgres"]}}}`)})
	output := collectAskOutput(t, c)
	if !strings.Contains(output, `User answers:\n- Database (Which database?): Postgres`) {
		t.Fatalf("ask item contents after correction = %q", output)
	}
}

func awaitQuestionRequest(t *testing.T, c *nativeClient) message {
	t.Helper()
	select {
	case request := <-c.requests:
		return request
	case <-time.After(waitLimit):
		t.Fatal("no native question request")
		return message{}
	}
}

func refuseBadAnswer(t *testing.T, c *nativeClient, threadID string, request message) {
	t.Helper()
	c.send(message{ID: request.ID, Result: []byte(`{"answers":{"q1":{"answers":["bad\u0000text"]}}}`)})
	warning := c.awaitEvent("warning", nil)
	want := map[string]string{
		"requestId": fieldOf(t, request.Params, "details", "requestId"), "field": "answers.q1.answers[0]", "reason": "disallowed_control",
		"message": "invalid text: answers.q1.answers[0]: disallowed_control", "threadId": threadID,
	}
	for key, value := range want {
		if got := fieldOf(t, warning.Params, key); got != value {
			t.Fatalf("warning %s = %q, want %q", key, got, value)
		}
	}
}

func collectAskOutput(t *testing.T, c *nativeClient) string {
	t.Helper()
	output := ""
	for _, event := range c.eventsUntil("turn/completed") {
		if event.Method == "serverRequest/resolved" && fieldOf(t, event.Params, "resolution") != "answered" {
			t.Fatalf("question resolved as %s", fieldOf(t, event.Params, "resolution"))
		}
		if event.Method != "item/completed" {
			continue
		}
		if header := eventItem(t, event); header.ID == "call-q" {
			for _, ref := range header.Contents {
				output += string(c.readContent(ref))
			}
		}
	}
	return output
}
