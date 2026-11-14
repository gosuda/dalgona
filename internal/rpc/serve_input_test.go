package rpc

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/tool"
	asktool "github.com/gosuda/dalgona/internal/tool/ask"
)

const questionArgs = `{"questions":[{"header":"Database","question":"Which database?","options":[{"label":"Postgres","description":"A server."},{"label":"SQLite","description":"One file."}]}]}`

type askOutcome struct {
	reply asktool.Reply
	err   error
}

type toolOutcome struct {
	result tool.Result
	err    error
}

func testQuestionList() []asktool.Question {
	return []asktool.Question{{
		Header: "Database", Text: "Which database?",
		Options: []asktool.Option{{Label: "Postgres", Description: "A server."}, {Label: "SQLite", Description: "One file."}},
	}}
}

func askInBackground(ctx context.Context, thread *agent.Thread) <-chan askOutcome {
	out := make(chan askOutcome, 1)
	go func() {
		reply, err := thread.Ask(ctx, "call-q", testQuestionList())
		out <- askOutcome{reply: reply, err: err}
	}()
	return out
}

func awaitAsk(t *testing.T, results <-chan askOutcome) asktool.Reply {
	t.Helper()
	select {
	case outcome := <-results:
		if outcome.err != nil {
			t.Fatalf("Ask: %v", outcome.err)
		}
		return outcome.reply
	case <-time.After(waitLimit):
		t.Fatal("Ask did not return")
	}
	return asktool.Reply{}
}

func awaitInputRequest(t *testing.T, c *wireClient) message {
	t.Helper()
	select {
	case m := <-c.requests:
		if m.Method != methodRequestUserInput {
			t.Fatalf("server request %s, want %s", m.Method, methodRequestUserInput)
		}
		return m
	case <-time.After(waitLimit):
		t.Fatal("no item/tool/requestUserInput request")
	}
	return message{}
}

func startQuestionThread(t *testing.T, r *rig, c *wireClient) *agent.Thread {
	t.Helper()
	thread, err := r.host.Start(t.Context(), agent.ThreadSpec{Cwd: r.cwd})
	if err != nil {
		t.Fatal(err)
	}
	var resumed ThreadResumeResponse
	c.ok("thread/resume", map[string]any{"threadId": thread.ID()}, &resumed)
	return thread
}

func checkQuestionParams(t *testing.T, raw jsontext.Value) {
	t.Helper()
	var params ToolRequestUserInputParams
	if err := json.Unmarshal(raw, &params); err != nil {
		t.Fatalf("decode params: %v", err)
	}
	if params.ItemID != "call-q" || !params.IsBlocking || len(params.Questions) != 1 {
		t.Fatalf("params = %+v", params)
	}
	if q := params.Questions[0]; q.ID != "q1" || !q.IsOther || q.Options[0].Label != "Postgres" {
		t.Fatalf("question = %+v", q)
	}
}

func arrivalIndex(c *wireClient, match func(message) bool) int {
	c.mu.Lock()
	defer c.mu.Unlock()
	for i, m := range c.arrivals {
		if match(m) {
			return i
		}
	}
	return -1
}

func TestServeQuestionRoundTrip(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	thread := startQuestionThread(t, r, c)
	results := askInBackground(t.Context(), thread)
	req := awaitInputRequest(t, c)
	checkQuestionParams(t, req.Params)
	waiting := arrivalIndex(c, func(m message) bool {
		return m.Method == "thread/status/changed" && strings.Contains(string(m.Params), "waitingOnUserInput")
	})
	requested := arrivalIndex(c, func(m message) bool { return m.Method == methodRequestUserInput })
	if waiting < 0 || requested < 0 || waiting > requested {
		t.Fatalf("waitingOnUserInput status arrived at %d, question request at %d; want the status first", waiting, requested)
	}
	c.respond(req.ID, `{"answers":{"q1":{"answers":["Postgres"]}}}`)
	reply := awaitAsk(t, results)
	if reply.Outcome != asktool.OutcomeAnswered || reply.Answers[0][0] != "Postgres" {
		t.Fatalf("reply = %+v, want Postgres", reply)
	}
}

func TestServeQuestionFirstWindowWins(t *testing.T) {
	r := newDalgonaRig(t)
	first := r.connect()
	thread := startQuestionThread(t, r, first)
	second := r.connect()
	var resumed ThreadResumeResponse
	second.ok("thread/resume", map[string]any{"threadId": thread.ID()}, &resumed)
	results := askInBackground(t.Context(), thread)
	reqFirst := awaitInputRequest(t, first)
	_ = awaitInputRequest(t, second)
	first.respond(reqFirst.ID, `{"answers":{"q1":{"answers":["SQLite"]}}}`)
	if reply := awaitAsk(t, results); reply.Answers[0][0] != "SQLite" {
		t.Fatalf("reply = %+v, want SQLite", reply)
	}
	second.await("serverRequest/resolved", func(m message) bool {
		return fieldOf(t, m.Params, "resolution") == "answered"
	})
}

func TestServeQuestionCancelByEmptyAnswers(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	thread := startQuestionThread(t, r, c)
	run := func(answer string) string {
		done := make(chan toolOutcome, 1)
		go func() {
			result, err := asktool.New(thread).Run(t.Context(), tool.Call{ID: "call-q", Name: "ask", Args: jsontext.Value(questionArgs)}, tool.Env{})
			done <- toolOutcome{result: result, err: err}
		}()
		req := awaitInputRequest(t, c)
		c.respond(req.ID, answer)
		select {
		case outcome := <-done:
			if outcome.err != nil {
				t.Fatalf("ask tool: %v", outcome.err)
			}
			return string(outcome.result.Output)
		case <-time.After(waitLimit):
			t.Fatal("ask tool did not return")
		}
		return ""
	}
	if got := run(`{"answers":{}}`); got != asktool.TextCancelled {
		t.Fatalf("cancel output = %q, want %q", got, asktool.TextCancelled)
	}
	if got := run(`{"answers":{"q1":{"answers":[]}}}`); got != asktool.TextSkipped {
		t.Fatalf("skip output = %q, want %q", got, asktool.TextSkipped)
	}
}

func TestServeQuestionOtherErrorWaitsForOtherWindow(t *testing.T) {
	r := newDalgonaRig(t)
	first := r.connect()
	thread := startQuestionThread(t, r, first)
	second := r.connect()
	var resumed ThreadResumeResponse
	second.ok("thread/resume", map[string]any{"threadId": thread.ID()}, &resumed)
	results := askInBackground(t.Context(), thread)
	reqFirst := awaitInputRequest(t, first)
	reqSecond := awaitInputRequest(t, second)
	first.send(message{ID: reqFirst.ID, Error: &RPCError{Code: -32603, Message: "cannot show"}})
	second.respond(reqSecond.ID, `{"answers":{"q1":{"answers":["Postgres"]}}}`)
	if reply := awaitAsk(t, results); reply.Outcome != asktool.OutcomeAnswered || reply.Answers[0][0] != "Postgres" {
		t.Fatalf("reply = %+v, want Postgres from the second window", reply)
	}
}

func TestServeQuestionErrorWithOneWindowDeclines(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	thread := startQuestionThread(t, r, c)
	results := askInBackground(t.Context(), thread)
	req := awaitInputRequest(t, c)
	c.send(message{ID: req.ID, Error: &RPCError{Code: -32603, Message: "cannot show"}})
	if reply := awaitAsk(t, results); reply.Outcome != asktool.OutcomeNoClient {
		t.Fatalf("outcome = %v, want no client", reply.Outcome)
	}
}

func TestServeQuestionDeclinedWhenWindowLeaves(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	thread := startQuestionThread(t, r, c)
	results := askInBackground(t.Context(), thread)
	_ = awaitInputRequest(t, c)
	_ = c.conn.Close()
	if reply := awaitAsk(t, results); reply.Outcome != asktool.OutcomeNoClient {
		t.Fatalf("outcome = %v, want no client", reply.Outcome)
	}
}

func TestServeQuestionReplayedToLateWindow(t *testing.T) {
	r := newDalgonaRig(t)
	first := r.connect()
	thread := startQuestionThread(t, r, first)
	results := askInBackground(t.Context(), thread)
	reqFirst := awaitInputRequest(t, first)
	late := r.connect()
	var resumed ThreadResumeResponse
	late.ok("thread/resume", map[string]any{"threadId": thread.ID()}, &resumed)
	reqLate := awaitInputRequest(t, late)
	if string(reqLate.ID) != string(reqFirst.ID) {
		t.Fatalf("late window got request %s, want %s", reqLate.ID, reqFirst.ID)
	}
	late.respond(reqLate.ID, `{"answers":{"q1":{"answers":["Postgres"]}}}`)
	if reply := awaitAsk(t, results); reply.Outcome != asktool.OutcomeAnswered {
		t.Fatalf("outcome = %v, want answered", reply.Outcome)
	}
}

func TestServeQuestionInterruptedResolution(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	thread := startQuestionThread(t, r, c)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	results := askInBackground(ctx, thread)
	_ = awaitInputRequest(t, c)
	cancel()
	if reply := awaitAsk(t, results); reply.Outcome != asktool.OutcomeInterrupted {
		t.Fatalf("outcome = %v, want interrupted", reply.Outcome)
	}
	c.await("serverRequest/resolved", func(m message) bool {
		return fieldOf(t, m.Params, "resolution") == "interrupted"
	})
}

func TestServeQuestionUnreadableAnswer(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	thread := startQuestionThread(t, r, c)
	results := askInBackground(t.Context(), thread)
	req := awaitInputRequest(t, c)
	c.respond(req.ID, `{"answers":"nope"}`)
	reply := awaitAsk(t, results)
	if reply.Outcome != asktool.OutcomeFailed || !strings.Contains(reply.Detail, "the client sent an unreadable answer") {
		t.Fatalf("reply = %+v, want an unreadable-answer failure", reply)
	}
}

func TestServeAskToolEndToEnd(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	args := `{"questions":[{"header":"Database","question":"Which database?","options":[{"label":"SQLite","description":"One file."},{"label":"Postgres","description":"A server."}]}]}`
	r.llm.push(llmReply{body: sseCall("call-q", "ask", args)}, llmReply{body: sseText("done")})
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "ask me")
	req := awaitInputRequest(t, c)
	c.respond(req.ID, `{"answers":{"q1":{"answers":["Postgres"]}}}`)
	log := readTurn(t, c.collect())
	expect(t, log.status == statusCompleted, "turn status", log.status)
	item := findItem(log.completed, "call-q")
	want := "User answers:\n- Database (Which database?): Postgres"
	expect(t, item.Output == want, "ask output", item.Output)
}
