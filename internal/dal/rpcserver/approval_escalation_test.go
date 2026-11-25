package rpcserver

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"testing"
	"time"
)

func TestExecApprovalCarriesEscalationOnlyWhenEscalated(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	r.llm.push(
		llmReply{body: sseCall("call-plain", "exec", `{"command":"echo plain"}`)},
		llmReply{body: sseCall("call-raw", "exec", `{"command":"echo raw","escalate":true,"justification":"needs the host"}`)},
		llmReply{body: sseText("done")},
	)
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "run both")
	for _, want := range []struct {
		item      string
		escalated bool
	}{{"call-plain", false}, {"call-raw", true}} {
		var req message
		select {
		case req = <-c.requests:
		case <-time.After(waitLimit):
			t.Fatalf("no approval request for %s", want.item)
		}
		member, present := escalatedMember(t, req.Params)
		if fieldOf(t, req.Params, "itemId") != want.item || present != want.escalated || want.escalated && member != "true" {
			t.Fatalf("approval for %s carries escalated %q (present %v); want present %v", fieldOf(t, req.Params, "itemId"), member, present, want.escalated)
		}
		c.respond(req.ID, `{"decision":"decline"}`)
	}
}

func escalatedMember(t *testing.T, params jsontext.Value) (string, bool) {
	t.Helper()
	var members map[string]jsontext.Value
	if err := json.Unmarshal(params, &members); err != nil {
		t.Fatal(err)
	}
	value, ok := members["escalated"]
	return string(value), ok
}
