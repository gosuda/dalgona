package script_test

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/script"
)

func jsEval(t *testing.T, j *journal) *script.Eval {
	t.Helper()
	return script.NewEval(j, script.EvalOptions{Mode: config.ToolModeEvalOnly, Tools: threadTools})
}

func quoteArgs(text string) string {
	quoted, _ := jsontext.AppendQuote(nil, text)
	return string(quoted)
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

func TestJSCellUnserializableResultIsNull(t *testing.T) {
	eval := jsEval(t, newJournal(echoTool))
	for i, code := range []string{"1n", "const x = {}; x.self = x; x"} {
		call := "call-" + string(rune('1'+i))
		got := runEval(t.Context(), t, eval, call, `{"lang":"js","code":`+quoteArgs(code)+`}`)
		if got.Status != "completed" || string(got.Value) != "null" {
			t.Fatalf("code %q: reply = %+v error = %+v", code, got, got.Error)
		}
	}
}

func TestJSCellDataKeepsProtoKeys(t *testing.T) {
	eval := jsEval(t, newJournal(echoTool))
	got := runEval(t.Context(), t, eval, "call-1", `{"lang":"js","code":"Object.hasOwn(data, '__proto__') && data.__proto__.role","data":{"__proto__":{"role":"admin"}}}`)
	if got.Status != "completed" || string(got.Value) != `"admin"` {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
}

func TestJSCellTimeAndRandomReplayIdentically(t *testing.T) {
	j := newJournal(echoTool)
	eval := jsEval(t, j)
	args := `{"lang":"js","code":"tools.read({path: 't' + Date.now() + 'r' + Math.random()})"}`
	got := runEval(t.Context(), t, eval, "call-1", args)
	if got.Status != "completed" {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
	got = runEval(t.Context(), t, eval, "call-1", args)
	if got.Status != "completed" {
		t.Fatalf("replay = %+v error = %+v; journaled time and seeded random must replay", got, got.Error)
	}
}

func TestJSCellDateIsCallableAndConstructible(t *testing.T) {
	eval := jsEval(t, newJournal(echoTool))
	got := runEval(t.Context(), t, eval, "call-1", `{"lang":"js","code":"typeof Date() === 'string' && new Date() instanceof Date && typeof Date.now() === 'number'"}`)
	if got.Status != "completed" || string(got.Value) != "true" {
		t.Fatalf("reply = %+v error = %+v", got, got.Error)
	}
}

func TestJSCellSpawnDecodesRunnerAndModel(t *testing.T) {
	j := newJournal(echoTool)
	eval := jsEval(t, j)
	got := runEval(t.Context(), t, eval, "call-1", `{"lang":"js","code":"ctx.spawn('task', {runner:'remote', model:'chosen'})"}`)
	if got.Status != "completed" || len(j.spawns) != 1 || j.spawns[0] != (script.SpawnOpts{Runner: "remote", Model: "chosen"}) {
		t.Fatalf("reply = %+v spawns = %+v", got, j.spawns)
	}
}

func TestJSCellBareToolCallArgIsAHostErrorNotAPanic(t *testing.T) {
	eval := jsEval(t, newJournal(echoTool))
	got := runEval(t.Context(), t, eval, "call-1", `{"lang":"js","code":"__toolCall()"}`)
	if got.Status != "failed" {
		t.Fatalf("reply = %+v; an arg-less host call must fail, not crash", got)
	}
}
