package script_test

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/script"
)

func jsEval(t *testing.T, j *journal) *script.Eval {
	t.Helper()
	return script.NewEval(j, script.EvalOptions{Mode: config.ToolModeEvalOnly, Tools: threadTools})
}

func TestJSCellReturnsLastExpressionAndPrints(t *testing.T) {
	j := newJournal(echoTool)
	eval := jsEval(t, j)
	got := runEval(t.Context(), t, eval, "call-1", `{"lang":"js","code":"console.log('a'); console.log('b');\ndata.n * 2","data":{"n":21}}`)
	if got.Status != "completed" || string(got.Value) != "42" || got.InvocationID != "call-1" {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
	if !slices.Equal(got.Prints.Lines, []string{"a", "b"}) || got.Prints.Truncated {
		t.Fatalf("prints = %+v", got.Prints)
	}
	if done := j.completed["call-1"]; done.Status != script.StatusCompleted || string(done.Value) != "42" {
		t.Fatalf("completed outcome = %+v", done)
	}
}

func TestJSCellSyntaxErrorStopsAtParseStage(t *testing.T) {
	eval := jsEval(t, newJournal(echoTool))
	got := runEval(t.Context(), t, eval, "call-1", `{"lang":"js","code":"const x = ("}`)
	if got.Status != "failed" || errorCode(got) != "syntax" || got.Error.Stage != "parse" {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
}

func TestJSCellToolCallsAreRecordedAndReplayed(t *testing.T) {
	j := newJournal(echoTool)
	eval := jsEval(t, j)
	args := `{"lang":"js","code":"const a = tools.read({path:'x'});\nconst b = tools.exec({command:'make'});\n[a, b]"}`
	got := runEval(t.Context(), t, eval, "call-1", args)
	if got.Status != "completed" {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
	executed, _ := j.counts()
	if executed != 2 {
		t.Fatalf("executed %d calls", executed)
	}
	got = runEval(t.Context(), t, eval, "call-1", args)
	if got.Status != "completed" {
		t.Fatalf("replayed reply = %+v error = %+v", got, got.Error)
	}
	executed, _ = j.counts()
	if executed != 2 {
		t.Fatalf("replay executed %d calls; recorded effects must not run again", executed)
	}
}

func TestJSCellToolOutsideUsesIsDenied(t *testing.T) {
	j := newJournal(echoTool)
	eval := jsEval(t, j)
	got := runEval(t.Context(), t, eval, "call-1", `{"lang":"js","code":"const r = ctx.tryCall(tools.exec, {command:'ls'});\nr","uses":[]}`)
	if errorCode(got) != "scope_exceeded" || got.Status != "denied" {
		t.Fatalf("reply = %+v, want scope_exceeded that tryCall cannot catch", got)
	}
	if executed, _ := j.counts(); executed != 0 {
		t.Fatalf("executed %d tool calls under uses = []", executed)
	}
}

func TestJSCellTryCallReturnsErrorStruct(t *testing.T) {
	j := newJournal(func(_ context.Context, _ string, _ jsontext.Value) (jsontext.Value, error) {
		return nil, errors.New("boom")
	})
	eval := jsEval(t, j)
	got := runEval(t.Context(), t, eval, "call-1", `{"lang":"js","code":"const r = ctx.tryCall(tools.read, {path:'x'});\nr.ok === false && r.error.includes('boom')"}`)
	if got.Status != "completed" || string(got.Value) != "true" {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
}

func TestJSCellUndefinedResultIsNull(t *testing.T) {
	eval := jsEval(t, newJournal(echoTool))
	got := runEval(t.Context(), t, eval, "call-1", `{"lang":"js","code":"const x = 1;"}`)
	if got.Status != "completed" || string(got.Value) != "null" {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
}
