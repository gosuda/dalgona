package advisor_test

import (
	"sync"
	"sync/atomic"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/advisor"
)

func TestNormalize(t *testing.T) {
	t.Parallel()
	cases := []struct{ in, want string }{
		{in: "Stop.", want: "stop"},
		{in: "  **STOP!!**  ", want: "stop"},
		{in: "No issue; continue.", want: "no issue continue"},
		{in: "Use `foo_bar()` -- now", want: "use foo bar now"},
		{in: "line one\n\tline two", want: "line one line two"},
		{in: "ＲＥＡＤ the file", want: "read the file"},
		{in: "the \uFB01le", want: "the file"},
		{in: "\u210Cello", want: "hello"},
		{in: "Chapter \u216B", want: "chapter xii"},
		{in: "Cafe\u0301 vs Café", want: "café vs café"},
		{in: "v2.0 fails", want: "v2 0 fails"},
		{in: "읽기 먼저, 쓰기 나중!", want: "읽기 먼저 쓰기 나중"},
		{in: "", want: ""},
		{in: " !!! ... --- ", want: ""},
	}
	for _, test := range cases {
		if got := advisor.Normalize(test.in); got != test.want {
			t.Errorf("Normalize(%q) = %q, want %q", test.in, got, test.want)
		}
	}
}

type admitStep struct {
	note          string
	severity      advisor.Severity
	want          advisor.Verdict
	wantDisplaced string
}

func runAdmits(t *testing.T, guard *advisor.Guard, steps []admitStep) {
	t.Helper()
	for i, step := range steps {
		got := guard.Admit(step.note, step.severity)
		if got.Verdict != step.want || got.Displaced != step.wantDisplaced {
			t.Fatalf("step %d Admit(%q, %s) = %+v, want verdict %s displaced %q", i, step.note, step.severity, got, step.want, step.wantDisplaced)
		}
	}
}

func TestGuard_rejections(t *testing.T) {
	t.Parallel()
	guard := advisor.NewGuard(2)
	runAdmits(t, guard, []admitStep{
		{note: "  ... ", severity: advisor.SeverityBlocker, want: advisor.VerdictNoise},
		{note: "Check the error path.", severity: advisor.SeverityNit, want: advisor.VerdictAccepted},
		{note: "check THE error-path!", severity: advisor.SeverityNit, want: advisor.VerdictDuplicate},
		{note: "Add a test.", severity: advisor.SeverityConcern, want: advisor.VerdictAccepted},
		{note: "add a test", severity: advisor.SeverityNit, want: advisor.VerdictDuplicate},
		{note: "Third nit.", severity: advisor.SeverityNit, want: advisor.VerdictBudget},
		{note: "Run the race detector.", severity: advisor.SeverityBlocker, want: advisor.VerdictAccepted},
		{note: "Second blocker is exempt too.", severity: advisor.SeverityBlocker, want: advisor.VerdictAccepted},
		{note: "run the race detector", severity: advisor.SeverityBlocker, want: advisor.VerdictDuplicate},
	})
}

func TestGuard_budgetRejectionDoesNotRecord(t *testing.T) {
	t.Parallel()
	guard := advisor.NewGuard(1)
	runAdmits(t, guard, []admitStep{
		{note: "first", severity: advisor.SeverityNit, want: advisor.VerdictAccepted},
		{note: "second", severity: advisor.SeverityNit, want: advisor.VerdictBudget},
	})
	guard.BeginReview()
	runAdmits(t, guard, []admitStep{
		{note: "second", severity: advisor.SeverityNit, want: advisor.VerdictAccepted},
		{note: "first", severity: advisor.SeverityNit, want: advisor.VerdictDuplicate},
	})
}

func TestGuard_escalationIsNotDuplicate(t *testing.T) {
	t.Parallel()
	guard := advisor.NewGuard(3)
	runAdmits(t, guard, []admitStep{
		{note: "Handle EOF.", severity: advisor.SeverityNit, want: advisor.VerdictAccepted},
		{note: "handle eof", severity: advisor.SeverityConcern, want: advisor.VerdictAccepted, wantDisplaced: "handle eof"},
		{note: "Handle EOF!", severity: advisor.SeverityConcern, want: advisor.VerdictDuplicate},
		{note: "HANDLE EOF", severity: advisor.SeverityBlocker, want: advisor.VerdictAccepted, wantDisplaced: "handle eof"},
		{note: "handle eof", severity: advisor.SeverityBlocker, want: advisor.VerdictDuplicate},
	})
}

func TestGuard_displacement(t *testing.T) {
	t.Parallel()
	t.Run("higher severity displaces lowest pending", func(t *testing.T) {
		t.Parallel()
		guard := advisor.NewGuard(2)
		runAdmits(t, guard, []admitStep{
			{note: "concern one", severity: advisor.SeverityConcern, want: advisor.VerdictAccepted},
			{note: "nit one", severity: advisor.SeverityNit, want: advisor.VerdictAccepted},
			{note: "concern two", severity: advisor.SeverityConcern, want: advisor.VerdictAccepted, wantDisplaced: "nit one"},
			{note: "concern three", severity: advisor.SeverityConcern, want: advisor.VerdictBudget},
			{note: "nit one", severity: advisor.SeverityNit, want: advisor.VerdictDuplicate},
		})
	})
	t.Run("equal severity never displaces", func(t *testing.T) {
		t.Parallel()
		guard := advisor.NewGuard(1)
		runAdmits(t, guard, []admitStep{
			{note: "nit one", severity: advisor.SeverityNit, want: advisor.VerdictAccepted},
			{note: "nit two", severity: advisor.SeverityNit, want: advisor.VerdictBudget},
		})
	})
	t.Run("delivered note keeps its slot", func(t *testing.T) {
		t.Parallel()
		guard := advisor.NewGuard(1)
		runAdmits(t, guard, []admitStep{{note: "nit one", severity: advisor.SeverityNit, want: advisor.VerdictAccepted}})
		guard.Delivered("nit one")
		runAdmits(t, guard, []admitStep{
			{note: "concern one", severity: advisor.SeverityConcern, want: advisor.VerdictBudget},
			{note: "nit one", severity: advisor.SeverityConcern, want: advisor.VerdictAccepted},
			{note: "concern two", severity: advisor.SeverityConcern, want: advisor.VerdictBudget},
		})
	})
	t.Run("earlier review notes are not displaced", func(t *testing.T) {
		t.Parallel()
		guard := advisor.NewGuard(1)
		runAdmits(t, guard, []admitStep{{note: "old nit", severity: advisor.SeverityNit, want: advisor.VerdictAccepted}})
		guard.BeginReview()
		runAdmits(t, guard, []admitStep{
			{note: "new nit", severity: advisor.SeverityNit, want: advisor.VerdictAccepted},
			{note: "new concern", severity: advisor.SeverityConcern, want: advisor.VerdictAccepted, wantDisplaced: "new nit"},
		})
	})
	t.Run("pending blocker escalation frees the slot", func(t *testing.T) {
		t.Parallel()
		guard := advisor.NewGuard(1)
		runAdmits(t, guard, []admitStep{
			{note: "nit one", severity: advisor.SeverityNit, want: advisor.VerdictAccepted},
			{note: "nit one", severity: advisor.SeverityBlocker, want: advisor.VerdictAccepted, wantDisplaced: "nit one"},
			{note: "nit two", severity: advisor.SeverityNit, want: advisor.VerdictAccepted},
		})
	})
	t.Run("delivered blocker escalation keeps the slot charged", func(t *testing.T) {
		t.Parallel()
		guard := advisor.NewGuard(1)
		runAdmits(t, guard, []admitStep{{note: "nit one", severity: advisor.SeverityNit, want: advisor.VerdictAccepted}})
		guard.Delivered("nit one")
		runAdmits(t, guard, []admitStep{
			{note: "nit one", severity: advisor.SeverityBlocker, want: advisor.VerdictAccepted},
			{note: "nit two", severity: advisor.SeverityNit, want: advisor.VerdictBudget},
		})
	})
	t.Run("zero budget admits only blockers", func(t *testing.T) {
		t.Parallel()
		guard := advisor.NewGuard(0)
		runAdmits(t, guard, []admitStep{
			{note: "concern", severity: advisor.SeverityConcern, want: advisor.VerdictBudget},
			{note: "blocker", severity: advisor.SeverityBlocker, want: advisor.VerdictAccepted},
		})
	})
}

func TestGuard_reset(t *testing.T) {
	t.Parallel()
	guard := advisor.NewGuard(1)
	runAdmits(t, guard, []admitStep{{note: "same note", severity: advisor.SeverityBlocker, want: advisor.VerdictAccepted}})
	guard.BeginReview()
	runAdmits(t, guard, []admitStep{{note: "same note", severity: advisor.SeverityNit, want: advisor.VerdictDuplicate}})
	guard.Reset()
	runAdmits(t, guard, []admitStep{
		{note: "same note", severity: advisor.SeverityNit, want: advisor.VerdictAccepted},
		{note: "other note", severity: advisor.SeverityNit, want: advisor.VerdictBudget},
	})
}

func TestGuard_concurrentAdmitsAcceptOnce(t *testing.T) {
	t.Parallel()
	guard := advisor.NewGuard(4)
	var accepted atomic.Int32
	var group sync.WaitGroup
	for range 64 {
		group.Go(func() {
			if guard.Admit("Same advice.", advisor.SeverityConcern).Verdict == advisor.VerdictAccepted {
				accepted.Add(1)
			}
		})
	}
	group.Wait()
	if got := accepted.Load(); got != 1 {
		t.Fatalf("accepted %d copies, want 1", got)
	}
}

func TestParseSeverity(t *testing.T) {
	t.Parallel()
	for _, severity := range []advisor.Severity{advisor.SeverityNit, advisor.SeverityConcern, advisor.SeverityBlocker} {
		parsed, ok := advisor.ParseSeverity(severity.String())
		if !ok || parsed != severity {
			t.Fatalf("ParseSeverity(%q) = %v, %v", severity.String(), parsed, ok)
		}
	}
	if _, ok := advisor.ParseSeverity("Blocker"); ok {
		t.Fatal("ParseSeverity accepted a non-canonical name")
	}
}
