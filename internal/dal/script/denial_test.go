package script_test

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/script"
)

func TestTryCallFailureWithDeniedPrefixReplaysCompleted(t *testing.T) {
	failure := errors.New("denied: disk quota exceeded")
	j := newJournal(func(context.Context, string, jsontext.Value) (jsontext.Value, error) {
		return nil, failure
	})
	src := "r = ctx.try_call(tools.exec, command = \"x\")\n[r.ok, r.error]"
	live := runDenialCell(t, j, src)
	if live.Status != "completed" {
		t.Fatalf("live status = %s, error = %+v", live.Status, live.Error)
	}
	replayed := runDenialCell(t, j, src)
	if replayed.Status != "completed" {
		t.Fatalf("replayed status = %s, want completed; error = %+v", replayed.Status, replayed.Error)
	}
	if string(replayed.Value) != string(live.Value) {
		t.Fatalf("replayed value = %s, want live value %s", replayed.Value, live.Value)
	}
	if !strings.Contains(string(replayed.Value), "denied: disk quota exceeded") {
		t.Fatalf("replayed value = %s, want the genuine failure text", replayed.Value)
	}
}

func runDenialCell(t *testing.T, j *journal, src string) reply {
	t.Helper()
	return runSharedCell(t.Context(), t, j, "run-1", src, "exec")
}

func TestReplayedDenialInsideTryCallHaltsDenied(t *testing.T) {
	j := newJournal(clockTool())
	src := "r = ctx.try_call(tools.exec, command = \"x\")\n42"
	if got := runDenialCell(t, j, src); got.Status != "completed" {
		t.Fatalf("first run status = %s, error = %+v", got.Status, got.Error)
	}
	j.rewrite(1, func(rec *script.Recorded) {
		rec.Result = nil
		rec.Error = "hooks forbid exec"
		rec.Denied = true
	})

	got := runDenialCell(t, j, src)
	if got.Status != "denied" {
		t.Fatalf("replayed status = %s, want denied; error = %+v, value = %s", got.Status, got.Error, got.Value)
	}
	if !strings.Contains(got.Error.Message, "hooks forbid exec") {
		t.Fatalf("replayed error = %+v, want the recorded denial reason", got.Error)
	}
}

func TestReplayedDeniedDirectCallHaltsDenied(t *testing.T) {
	j := newJournal(clockTool())
	src := "tools.exec(command = \"x\")"
	if got := runDenialCell(t, j, src); got.Status != "completed" {
		t.Fatalf("first run status = %s, error = %+v", got.Status, got.Error)
	}
	j.rewrite(1, func(rec *script.Recorded) {
		rec.Result = nil
		rec.Error = "hooks forbid exec"
		rec.Denied = true
	})

	got := runDenialCell(t, j, src)
	if got.Status != "denied" || got.Error.Code != "denied" {
		t.Fatalf("replayed status = %s, code = %s, want denied/denied", got.Status, got.Error.Code)
	}
}
