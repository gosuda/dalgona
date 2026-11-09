package script_test

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"strconv"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/script"
)

const replayWorkflow = `
a = ctx.tools.exec(command = "date +%s%N")
b = ctx.tools.exec(command = "date +%s%N")
c = ctx.tools.exec(command = "date +%s%N")
t = time.time()
[a["output"], b["output"], c["output"], t]
`

func clockTool() toolFunc {
	var counter atomic.Int64
	return func(_ context.Context, _ string, _ jsontext.Value) (jsontext.Value, error) {
		return jsontext.Value(`{"output":"` + strconv.FormatInt(counter.Add(1), 10) + `"}`), nil
	}
}

func workflowValues(t *testing.T, outcome script.Outcome) []any {
	t.Helper()
	if outcome.Status != script.StatusCompleted {
		t.Fatalf("status = %s, error = %+v", outcome.Status, outcome.Error)
	}
	var values []any
	if err := json.Unmarshal(outcome.Value, &values); err != nil {
		t.Fatalf("decode value %s: %v", outcome.Value, err)
	}
	return values
}

func runReplayWorkflow(ctx context.Context, t *testing.T, j *journal) script.Outcome {
	t.Helper()
	outcome, err := script.RunWorkflow(ctx, j, script.Workflow{ID: "run-1", Entry: "replay.star", Source: []byte(replayWorkflow)})
	if err != nil {
		t.Fatalf("run workflow: %v", err)
	}
	return outcome
}

func killedAfterSecondEffect(t *testing.T) *journal {
	t.Helper()
	j := newJournal(clockTool())
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	j.killBefore, j.kill = 3, cancel
	outcome := runReplayWorkflow(ctx, t, j)
	if outcome.Status != script.StatusCancelled {
		t.Fatalf("killed run status = %s, want cancelled", outcome.Status)
	}
	if executed, _ := j.counts(); executed != 2 {
		t.Fatalf("killed run executed %d calls, want 2", executed)
	}
	return j
}

func recordedOutput(t *testing.T, j *journal, n int) string {
	t.Helper()
	rec, ok := j.Lookup("run-1", n)
	if !ok {
		t.Fatalf("effect %d missing from journal", n)
	}
	var out struct {
		Output string `json:"output"`
	}
	if err := json.Unmarshal(rec.Result, &out); err != nil {
		t.Fatalf("decode effect %d: %v", n, err)
	}
	return out.Output
}

func TestRecoveredWorkflowReplaysJournalAndRunsOneNewExec(t *testing.T) {
	j := killedAfterSecondEffect(t)
	first, second := recordedOutput(t, j, 1), recordedOutput(t, j, 2)

	values := workflowValues(t, runReplayWorkflow(t.Context(), t, j))
	if values[0] != first || values[1] != second {
		t.Fatalf("replayed results = %v, %v; journal has %s, %s", values[0], values[1], first, second)
	}
	if executed, _ := j.counts(); executed != 3 {
		t.Fatalf("executed %d calls after recovery, want exactly one new exec (3 total)", executed)
	}
	if values[2] == first || values[2] == second {
		t.Fatalf("third exec reused a journaled result: %v", values)
	}

	again := workflowValues(t, runReplayWorkflow(t.Context(), t, j))
	if executed, _ := j.counts(); executed != 3 {
		t.Fatalf("a full replay executed %d calls, want 3", executed)
	}
	for i := range values {
		if again[i] != values[i] {
			t.Fatalf("full replay value %d = %v, want %v (time.time must replay)", i, again[i], values[i])
		}
	}
}

func TestSwappedJournalOpReportsNondeterminism(t *testing.T) {
	j := killedAfterSecondEffect(t)
	j.rewrite("run-1", 2, func(rec *script.Recorded) { rec.Op = script.OpTime })

	outcome := runReplayWorkflow(t.Context(), t, j)
	want := "nondeterminism at effect 2: recorded time, got tool:exec"
	if outcome.Status != script.StatusFailed || outcome.Error.Message != want {
		t.Fatalf("outcome = %s %+v, want failed %q", outcome.Status, outcome.Error, want)
	}
	if executed, _ := j.counts(); executed != 2 {
		t.Fatalf("divergent run executed %d calls, want none past the divergence", executed)
	}
}

func TestChangedArgumentsReportNondeterminism(t *testing.T) {
	j := killedAfterSecondEffect(t)
	j.rewrite("run-1", 1, func(rec *script.Recorded) { rec.ArgsSHA = script.ArgsSHA(jsontext.Value(`{"command":"true"}`)) })

	outcome := runReplayWorkflow(t.Context(), t, j)
	if outcome.Error.Message != "nondeterminism at effect 1: recorded tool:exec, got tool:exec" {
		t.Fatalf("outcome = %s %+v", outcome.Status, outcome.Error)
	}
}

func TestSleepRecordsAbsoluteDeadline(t *testing.T) {
	j := newJournal(clockTool())
	src := []byte("ctx.sleep(0.05)\ntime.sleep(0)\n")
	outcome, err := script.RunWorkflow(t.Context(), j, script.Workflow{ID: "sleepy", Entry: "s.star", Source: src})
	if err != nil || outcome.Status != script.StatusCompleted {
		t.Fatalf("run: %v %+v", err, outcome)
	}
	rec, ok := j.Lookup("sleepy", 1)
	if !ok || rec.Op != script.OpSleep {
		t.Fatalf("sleep effect = %+v, %v", rec, ok)
	}
	deadline, err := strconv.ParseInt(string(rec.Result), 10, 64)
	if err != nil || deadline < time.Now().Add(-time.Minute).UnixNano() {
		t.Fatalf("deadline %s is not an absolute unix time: %v", rec.Result, err)
	}
	if _, ok := j.Lookup("sleepy", 2); !ok {
		t.Fatal("time.sleep did not record a durable effect")
	}
}
