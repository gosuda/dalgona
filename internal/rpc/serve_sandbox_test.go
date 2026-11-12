package rpc

import (
	json "encoding/json/v2"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/config"
)

const sandboxCause = "Landlock is not available on this kernel"

const sandboxWarning = "Sandbox unavailable: Landlock is not available on this kernel. Choose how commands in this thread run."

func newSandboxRig(t *testing.T) *rig {
	t.Helper()
	r := newRig(t, config.ApprovalAuto)
	r.cfg.SandboxCause = func() string { return sandboxCause }
	r.cfg.Config.Sandbox = config.SandboxWorkspaceWrite
	r.openHost()
	return r
}

func startSandboxTurn(t *testing.T, r *rig, c *wireClient) (string, message) {
	t.Helper()
	r.llm.push(llmReply{body: sseCall("call-x", "exec", `{"command":"echo one"}`)}, llmReply{body: sseText("done")})
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "run it")
	return threadID, awaitInputRequest(t, c)
}

func awaitRequestNamed(t *testing.T, c *wireClient, method string) message {
	t.Helper()
	select {
	case m := <-c.requests:
		if m.Method != method {
			t.Fatalf("server request %s, want %s", m.Method, method)
		}
		return m
	case <-time.After(waitLimit):
		t.Fatalf("no %s request", method)
	}
	return message{}
}

func TestWarningNotification(t *testing.T) {
	r := newSandboxRig(t)
	c := r.connect()
	threadID, question := startSandboxTurn(t, r, c)
	warning := c.await("warning", nil)
	if got := fieldOf(t, warning.Params, "threadId"); got != threadID {
		t.Fatalf("warning threadId = %q, want %q", got, threadID)
	}
	if got := fieldOf(t, warning.Params, "message"); got != sandboxWarning {
		t.Fatalf("warning message = %q, want %q", got, sandboxWarning)
	}
	warned := arrivalIndex(c, func(m message) bool { return m.Method == "warning" })
	requested := arrivalIndex(c, func(m message) bool { return m.Method == methodRequestUserInput })
	if warned < 0 || requested < 0 || warned > requested {
		t.Fatalf("warning arrived at %d and question at %d, want the warning first", warned, requested)
	}
	c.respond(question.ID, `{"answers":{}}`)
	c.await("turn/completed", nil)
}

func TestSandboxQuestionHasNoOtherRow(t *testing.T) {
	r := newSandboxRig(t)
	c := r.connect()
	_, question := startSandboxTurn(t, r, c)
	var params ToolRequestUserInputParams
	if err := json.Unmarshal(question.Params, &params); err != nil {
		t.Fatalf("decode question params: %v", err)
	}
	if len(params.Questions) != 1 {
		t.Fatalf("questions = %+v, want one sandbox question", params.Questions)
	}
	q := params.Questions[0]
	if q.Header != "Sandbox" || q.IsOther {
		t.Fatalf("sandbox question = %+v, want header Sandbox and no Other row", q)
	}
	if len(q.Options) != 2 || q.Options[0].Label != "Accept" || q.Options[1].Label != "Dangerously accept" {
		t.Fatalf("sandbox options = %+v, want Accept then Dangerously accept", q.Options)
	}
	c.respond(question.ID, `{"answers":{}}`)
	c.await("turn/completed", nil)
}

func TestSandboxChoiceOverTheWire(t *testing.T) {
	r := newSandboxRig(t)
	c := r.connect()
	threadID, question := startSandboxTurn(t, r, c)
	c.await("warning", nil)
	c.respond(question.ID, `{"answers":{"q1":{"answers":["Accept"]}}}`)
	chosen := c.await("dalgona/sandbox/chosen", nil)
	if got := fieldOf(t, chosen.Params, "threadId"); got != threadID {
		t.Fatalf("chosen threadId = %q, want %q", got, threadID)
	}
	if got := fieldOf(t, chosen.Params, "choice"); got != "accept" {
		t.Fatalf("chosen choice = %q, want accept", got)
	}
	if got := fieldOf(t, chosen.Params, "cause"); got != sandboxCause {
		t.Fatalf("chosen cause = %q, want %q", got, sandboxCause)
	}
	approval := awaitRequestNamed(t, c, "item/commandExecution/requestApproval")
	c.respond(approval.ID, `{"decision":"decline"}`)
	c.await("turn/completed", nil)
	resume := r.connect()
	var resumed ThreadResumeResponse
	resume.ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	if resumed.SandboxFallback == nil || resumed.SandboxFallback.Cause != sandboxCause {
		t.Fatalf("resume sandboxFallback = %+v, want cause %q", resumed.SandboxFallback, sandboxCause)
	}
	if resumed.SandboxFallback.Choice == nil || *resumed.SandboxFallback.Choice != "accept" {
		t.Fatalf("resume sandboxFallback choice = %+v, want accept", resumed.SandboxFallback.Choice)
	}
}
