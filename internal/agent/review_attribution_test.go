package agent

import (
	"errors"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

const attributionCause = "primary-model: decisions: http 429: rate limited"

func fallbackScores() map[string]float64 {
	return map[string]float64{"sound": 0.9, "lost_code": 0.02, "incomplete": 0.02}
}

func maskToken(text string) (string, error) {
	return strings.ReplaceAll(text, "SECRETVALUE", "{{secret:TOKEN}}"), nil
}

func requireFallbackAttribution(t *testing.T, got event.ToolJudged, by, wantFallback string) {
	t.Helper()
	if got.By != by || got.Fallback != wantFallback || got.Judges != nil {
		t.Fatalf("judged = %+v, want by %s with fallback %q and no judges", got, by, wantFallback)
	}
}

func runAttributedEdit(t *testing.T, judge *judgeStub, mask func(string) (string, error)) []event.ToolJudged {
	t.Helper()
	h := judgedHarness(t, judge, nil)
	if mask != nil {
		h.host.reviewMask = mask
	}
	h.write("a.txt", "one\ntwo\n")
	thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
	return judgedOf(h.events(thread.ID()))
}

func runAttributedParts(t *testing.T, judge *judgeStub) []event.ToolJudged {
	t.Helper()
	h := judgedHarness(t, judge, nil)
	file, pairs := blockEdits(3)
	h.write("a.go", file)
	thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.go", pairs...)}})
	return judgedOf(h.events(thread.ID()))
}

func requireOneJudged(t *testing.T, judged []event.ToolJudged) event.ToolJudged {
	t.Helper()
	if len(judged) != 1 {
		t.Fatalf("tool.judged count = %d, want 1: %+v", len(judged), judged)
	}
	return judged[0]
}

func requireJudgedParts(t *testing.T, got event.ToolJudged, want []event.JudgeAttribution) {
	t.Helper()
	if !slices.Equal(got.Judges, want) {
		t.Fatalf("judged = %+v, want part attribution %+v", got, want)
	}
}

func TestEditReviewJournalsFallbackAttribution(t *testing.T) {
	t.Parallel()
	t.Run("one part records the answering model and the primary failure", func(t *testing.T) {
		t.Parallel()
		got := requireOneJudged(t, runAttributedEdit(t, &judgeStub{model: "smol-model", fallback: attributionCause, scores: fallbackScores()}, nil))
		requireFallbackAttribution(t, got, "judge:smol-model", attributionCause)
	})
	t.Run("a primary answer records no fallback", func(t *testing.T) {
		t.Parallel()
		got := requireOneJudged(t, runAttributedEdit(t, &judgeStub{model: "primary-model", scores: fallbackScores()}, nil))
		requireFallbackAttribution(t, got, "judge:primary-model", "")
	})
	t.Run("the fallback text is masked", func(t *testing.T) {
		t.Parallel()
		judge := &judgeStub{model: "smol-model", fallback: "primary-model: token SECRETVALUE rejected", scores: fallbackScores()}
		got := requireOneJudged(t, runAttributedEdit(t, judge, maskToken))
		if want := "primary-model: token {{secret:TOKEN}} rejected"; got.Fallback != want {
			t.Fatalf("fallback = %q, want %q", got.Fallback, want)
		}
	})
	t.Run("a fallback that cannot be masked is not journaled as written", func(t *testing.T) {
		t.Parallel()
		mask := func(text string) (string, error) {
			if strings.Contains(text, "FALLBACKFAIL") {
				return "", errors.New("masker down")
			}
			return text, nil
		}
		judge := &judgeStub{model: "smol-model", fallback: "primary-model: FALLBACKFAIL", scores: fallbackScores()}
		got := requireOneJudged(t, runAttributedEdit(t, judge, mask))
		if got.Fallback == "" || strings.Contains(got.Fallback, "FALLBACKFAIL") || got.Verdict == "" {
			t.Fatalf("judged = %+v, want a verdict and a fallback text that holds the unmasked text nowhere", got)
		}
	})
	t.Run("an overlong fallback is cut and the verdict is still journaled", func(t *testing.T) {
		t.Parallel()
		judge := &judgeStub{model: "smol-model", fallback: strings.Repeat("x", 2*event.MaxJudgeErrorBytes), scores: fallbackScores()}
		got := requireOneJudged(t, runAttributedEdit(t, judge, nil))
		if len(got.Fallback) != event.MaxJudgeErrorBytes {
			t.Fatalf("fallback length = %d, want %d", len(got.Fallback), event.MaxJudgeErrorBytes)
		}
	})
}

func TestEditReviewJournalsPartAttribution(t *testing.T) {
	t.Parallel()
	t.Run("one answering model keeps one entry for each part", func(t *testing.T) {
		t.Parallel()
		got := requireOneJudged(t, runAttributedParts(t, &judgeStub{model: "smol-model", fallback: attributionCause, scores: fallbackScores()}))
		want := []event.JudgeAttribution{
			{Part: 0, By: "judge:smol-model", Fallback: attributionCause},
			{Part: 1, By: "judge:smol-model", Fallback: attributionCause},
		}
		if got.Parts != 2 || got.By != "judge:smol-model" || got.Fallback != attributionCause {
			t.Fatalf("judged = %+v, want two parts answered by smol-model with fallback %q", got, attributionCause)
		}
		requireJudgedParts(t, got, want)
	})
	t.Run("mixed answering models keep their order", func(t *testing.T) {
		t.Parallel()
		judge := &judgeStub{model: "smol-model", fallback: attributionCause, altWhen: "THIRDBLOCK", altModel: "primary-b", scores: fallbackScores()}
		got := requireOneJudged(t, runAttributedParts(t, judge))
		want := []event.JudgeAttribution{
			{Part: 0, By: "judge:smol-model", Fallback: attributionCause},
			{Part: 1, By: "judge:primary-b"},
		}
		if got.By != "judge:mixed" || got.Fallback != attributionCause {
			t.Fatalf("judged = %+v, want judge:mixed with fallback %q", got, attributionCause)
		}
		requireJudgedParts(t, got, want)
	})
	t.Run("parts with no fallback record none", func(t *testing.T) {
		t.Parallel()
		got := requireOneJudged(t, runAttributedParts(t, &judgeStub{model: "primary-model", scores: fallbackScores()}))
		want := []event.JudgeAttribution{{Part: 0, By: "judge:primary-model"}, {Part: 1, By: "judge:primary-model"}}
		if got.Fallback != "" {
			t.Fatalf("judged = %+v, want no fallback", got)
		}
		requireJudgedParts(t, got, want)
	})
}

func TestTurnReviewJournalsFallbackAttribution(t *testing.T) {
	t.Parallel()
	run := func(t *testing.T, judge *judgeStub) []event.TurnJudged {
		t.Helper()
		h := judgedHarness(t, judge, nil)
		h.write("a.txt", "one\n")
		thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
		return turnJudgedOf(h.events(thread.ID()))
	}
	t.Run("a fallback answer records the answering model and the primary failure", func(t *testing.T) {
		t.Parallel()
		turns := run(t, &judgeStub{model: "smol-model", fallback: attributionCause, scores: fallbackScores()})
		if len(turns) != 1 || turns[0].By != "judge:smol-model" || turns[0].Fallback != attributionCause || turns[0].Judges != nil {
			t.Fatalf("turn.judged = %+v, want by judge:smol-model, fallback %q and no judges", turns, attributionCause)
		}
	})
	t.Run("a primary answer records no fallback", func(t *testing.T) {
		t.Parallel()
		turns := run(t, &judgeStub{model: "primary-model", scores: fallbackScores()})
		if len(turns) != 1 || turns[0].By != "judge:primary-model" || turns[0].Fallback != "" || turns[0].Judges != nil {
			t.Fatalf("turn.judged = %+v, want by judge:primary-model and no attribution members", turns)
		}
	})
}
