package rpcserver

import (
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	asktool "github.com/gosuda/dalgona/internal/dal/tool/ask"
)

func overLimitAnswer() string {
	return `{"answers":{"q1":{"answers":["` + strings.Repeat("a", asktool.MaxAnswer+1) + `"]}}}`
}

func awaitCorrection(t *testing.T, c *wireClient, requestID string) message {
	t.Helper()
	warning := c.await("warning", func(m message) bool { return fieldOf(t, m.Params, "requestId") == requestID })
	want := map[string]string{
		"message": "invalid text: answers.q1.answers[0]: text_too_large",
		"reason":  "text_too_large",
		"field":   "answers.q1.answers[0]",
	}
	for key, value := range want {
		if got := fieldOf(t, warning.Params, key); got != value {
			t.Fatalf("warning %s = %q, want %q in %s", key, got, value, warning.Params)
		}
	}
	return warning
}

func requireNoResolution(t *testing.T, c *wireClient, results <-chan askOutcome) {
	t.Helper()
	select {
	case outcome := <-results:
		t.Fatalf("the question ended on invalid text: %+v", outcome)
	case <-time.After(100 * time.Millisecond):
	}
	if arrivalIndex(c, func(m message) bool { return m.Method == "serverRequest/resolved" }) >= 0 {
		t.Fatal("invalid text resolved the question")
	}
}

func TestServeQuestionAcceptsACorrectionUnderTheSameRequest(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	thread := startQuestionThread(t, r, c)
	results := askInBackground(t.Context(), thread)
	req := awaitInputRequest(t, c)
	requestID := strings.Trim(string(req.ID), `"`)
	c.respond(req.ID, overLimitAnswer())
	awaitCorrection(t, c, requestID)
	requireNoResolution(t, c, results)
	c.respond(req.ID, `{"answers":{"q1":{"answers":["  keep my spaces  "]}}}`)
	reply := awaitAsk(t, results)
	if reply.Outcome != asktool.OutcomeAnswered || len(reply.Answers[0]) != 1 || reply.Answers[0][0] != "  keep my spaces  " {
		t.Fatalf("reply = %+v, want the exact corrected answer", reply)
	}
	c.await("serverRequest/resolved", func(m message) bool { return fieldOf(t, m.Params, "resolution") == "answered" })
}

func TestServeQuestionCorrectionWarningReachesOnlyTheAnsweringWindow(t *testing.T) {
	r := newDalgonaRig(t)
	first := r.connect()
	thread := startQuestionThread(t, r, first)
	second := r.connect()
	var resumed rpc.ThreadResumeResponse
	second.ok("thread/resume", map[string]any{"threadId": thread.ID()}, &resumed)
	results := askInBackground(t.Context(), thread)
	reqFirst := awaitInputRequest(t, first)
	reqSecond := awaitInputRequest(t, second)
	first.respond(reqFirst.ID, overLimitAnswer())
	awaitCorrection(t, first, strings.Trim(string(reqFirst.ID), `"`))
	second.respond(reqSecond.ID, `{"answers":{"q1":{"answers":["SQLite"]}}}`)
	if reply := awaitAsk(t, results); reply.Outcome != asktool.OutcomeAnswered || reply.Answers[0][0] != "SQLite" {
		t.Fatalf("reply = %+v, want the second window's valid answer", reply)
	}
	if arrivalIndex(second, func(m message) bool { return m.Method == "warning" }) >= 0 {
		t.Fatal("the other window received the correction warning")
	}
	first.await("serverRequest/resolved", func(m message) bool { return fieldOf(t, m.Params, "resolution") == "answered" })
}
