package advisor_test

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/advisor"
)

func TestSystem_fillsBudgetAndAppendsSpecializationThenWatchdog(t *testing.T) {
	t.Parallel()
	watchdog := `<watchdog path="/w/WATCHDOG.md">Watch migrations.</watchdog>`
	got := advisor.System(7, "  Focus on auth.  ", watchdog)
	if strings.Contains(got, "{{") || !strings.Contains(got, "Send at most 7 `nit` or `concern` notes per review.") {
		t.Fatalf("budget not filled:\n%s", got)
	}
	suffix := "\n\n<specialization>\nFocus on auth.\n</specialization>\n\n" + watchdog
	if !strings.HasSuffix(got, suffix) {
		t.Fatalf("System suffix = %q, want %q", got[max(0, len(got)-len(suffix)-20):], suffix)
	}
	if bare := advisor.System(3, " ", ""); strings.Contains(bare, "<specialization>") || strings.HasSuffix(bare, "\n") {
		t.Fatalf("empty parts leaked into the prompt: %q", bare[len(bare)-40:])
	}
}

func TestAdviseDescription_namesEveryVerdict(t *testing.T) {
	t.Parallel()
	for _, verdict := range []advisor.Verdict{advisor.VerdictAccepted, advisor.VerdictDuplicate, advisor.VerdictNoise, advisor.VerdictBudget} {
		if !strings.Contains(advisor.AdviseDescription, "`"+string(verdict)+"`") {
			t.Fatalf("advise description does not name verdict %q", verdict)
		}
	}
}
