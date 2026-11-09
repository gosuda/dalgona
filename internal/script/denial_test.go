package script_test

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/script"
)

func TestTryCallFailureWithDeniedPrefixReplaysCompleted(t *testing.T) {
	failure := errors.New("denied: disk quota exceeded")
	j := newJournal(func(context.Context, string, jsontext.Value) (jsontext.Value, error) {
		return nil, failure
	})
	src := "r = ctx.try_call(tools.exec, command = \"x\")\n[r.ok, r.error]"
	live := runDenialWorkflow(t, j, src)
	if live.Status != script.StatusCompleted {
		t.Fatalf("live status = %s, error = %+v", live.Status, live.Error)
	}
	replayed := runDenialWorkflow(t, j, src)
	if replayed.Status != script.StatusCompleted {
		t.Fatalf("replayed status = %s, want completed; error = %+v", replayed.Status, replayed.Error)
	}
	if string(replayed.Value) != string(live.Value) {
		t.Fatalf("replayed value = %s, want live value %s", replayed.Value, live.Value)
	}
	if !strings.Contains(string(replayed.Value), "denied: disk quota exceeded") {
		t.Fatalf("replayed value = %s, want the genuine failure text", replayed.Value)
	}
}

func runDenialWorkflow(t *testing.T, j *journal, src string) script.Outcome {
	t.Helper()
	outcome, err := script.RunWorkflow(t.Context(), j, script.Workflow{ID: "run-1", Entry: "denial.star", Source: []byte(src)})
	if err != nil {
		t.Fatalf("run workflow: %v", err)
	}
	return outcome
}

func TestReplayedDenialInsideTryCallHaltsDenied(t *testing.T) {
	j := newJournal(clockTool())
	src := "r = ctx.try_call(tools.exec, command = \"x\")\n42"
	if outcome := runDenialWorkflow(t, j, src); outcome.Status != script.StatusCompleted {
		t.Fatalf("first run status = %s, error = %+v", outcome.Status, outcome.Error)
	}
	j.rewrite("run-1", 1, func(rec *script.Recorded) {
		rec.Result = nil
		rec.Error = "hooks forbid exec"
		rec.Denied = true
	})

	outcome := runDenialWorkflow(t, j, src)
	if outcome.Status != script.StatusDenied {
		t.Fatalf("replayed status = %s, want denied; error = %+v, value = %s", outcome.Status, outcome.Error, outcome.Value)
	}
	if !strings.Contains(outcome.Error.Message, "hooks forbid exec") {
		t.Fatalf("replayed error = %+v, want the recorded denial reason", outcome.Error)
	}
}

func TestReplayedDeniedDirectCallHaltsDenied(t *testing.T) {
	j := newJournal(clockTool())
	src := "tools.exec(command = \"x\")"
	if outcome := runDenialWorkflow(t, j, src); outcome.Status != script.StatusCompleted {
		t.Fatalf("first run status = %s, error = %+v", outcome.Status, outcome.Error)
	}
	j.rewrite("run-1", 1, func(rec *script.Recorded) {
		rec.Result = nil
		rec.Error = "hooks forbid exec"
		rec.Denied = true
	})

	outcome := runDenialWorkflow(t, j, src)
	if outcome.Status != script.StatusDenied || outcome.Error.Code != "denied" {
		t.Fatalf("replayed status = %s, code = %s, want denied/denied", outcome.Status, outcome.Error.Code)
	}
}
