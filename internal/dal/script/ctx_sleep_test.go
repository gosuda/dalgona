package script_test

import (
	"strconv"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/dal/script"
)

const sleepRefusal = "ctx.sleep: seconds must be non-negative and finite"

type sleepRun struct {
	journal *journal
	outcome reply
	started time.Time
	elapsed time.Duration
}

func runSleepScript(t *testing.T, src string) sleepRun {
	t.Helper()
	j := newJournal(echoTool)
	started := time.Now()
	got := runSharedCell(t.Context(), t, j, "sleep", src)
	return sleepRun{journal: j, outcome: got, started: started, elapsed: time.Since(started)}
}

func TestCtxSleepAcceptsIntegerAndFloatSeconds(t *testing.T) {
	cases := []struct {
		name string
		src  string
		wait time.Duration
	}{
		{"integer", "ctx.sleep(2)", 2 * time.Second},
		{"float", "ctx.sleep(2.0)", 2 * time.Second},
		{"keyword integer", "ctx.sleep(seconds = 2)", 2 * time.Second},
		{"fraction", "ctx.sleep(0.25)", 250 * time.Millisecond},
		{"zero integer", "ctx.sleep(0)", 0},
		{"zero float", "ctx.sleep(0.0)", 0},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				assertSlept(t, runSleepScript(t, tc.src), tc.wait)
			})
		})
	}
}

func assertSlept(t *testing.T, got sleepRun, wait time.Duration) {
	t.Helper()
	if got.outcome.Status != "completed" {
		t.Fatalf("status = %s, error = %+v, want completed", got.outcome.Status, got.outcome.Error)
	}
	if got.elapsed != wait {
		t.Fatalf("waited %v of fake time, want exactly %v", got.elapsed, wait)
	}
	rec, ok := got.journal.Lookup("sleep", 1)
	if !ok || rec.Op != script.OpSleep {
		t.Fatalf("effect 1 = %+v, %v, want one sleep effect", rec, ok)
	}
	if want := script.ArgsSHA(strconv.AppendInt(nil, int64(wait), 10)); rec.ArgsSHA != want {
		t.Fatalf("effect 1 args sha = %s, want the sha of %d ns", rec.ArgsSHA, int64(wait))
	}
	if want := strconv.FormatInt(got.started.Add(wait).UnixNano(), 10); string(rec.Result) != want {
		t.Fatalf("effect 1 deadline = %s, want %s", rec.Result, want)
	}
}

func TestCtxSleepRefusesWhatIsNotANonNegativeFiniteNumber(t *testing.T) {
	cases := []struct {
		name string
		src  string
		want string
	}{
		{"negative integer", "ctx.sleep(-1)", sleepRefusal},
		{"negative float", "ctx.sleep(-0.5)", sleepRefusal},
		{"nan", `ctx.sleep(float("nan"))`, sleepRefusal},
		{"infinity", `ctx.sleep(float("inf"))`, sleepRefusal},
		{"negative infinity", `ctx.sleep(-float("inf"))`, sleepRefusal},
		{"integer beyond float range", "ctx.sleep(1" + strings.Repeat("0", 400) + ")", sleepRefusal},
		{"string", `ctx.sleep("2")`, "ctx.sleep: for parameter seconds: got string, want number"},
		{"none", "ctx.sleep(None)", "ctx.sleep: for parameter seconds: got NoneType, want number"},
		{"bool", "ctx.sleep(True)", "ctx.sleep: for parameter seconds: got bool, want number"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				got := runSleepScript(t, tc.src)
				failure := got.outcome.Error
				if got.outcome.Status != "failed" || failure.Code != "error" || failure.Stage != "run" || failure.Message != tc.want {
					t.Fatalf("outcome = %s %+v, want failed error at run with message %q", got.outcome.Status, failure, tc.want)
				}
				if got.elapsed != 0 {
					t.Fatalf("refused call waited %v of fake time, want none", got.elapsed)
				}
				if rec, ok := got.journal.Lookup("sleep", 1); ok {
					t.Fatalf("refused call journaled effect %+v, want none", rec)
				}
			})
		})
	}
}
