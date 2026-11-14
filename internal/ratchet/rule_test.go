package ratchet

import (
	"fmt"
	"testing"
)

func mk(train, test string) pair {
	return pair{Train: delta{Verdict: train}, Test: delta{Verdict: test}}
}

func TestDecideRound(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name   string
		guard  pair
		target pair
		want   string
		reason string
	}{
		{"train guard regressed", mk("flat", "down"), mk("up", "up"), "revert", "guardrail regressed vs baseline: latency_s"},
		{"test guard regressed", mk("down", "flat"), mk("up", "up"), "revert", "guardrail regressed vs baseline: latency_s"},
		{"target train regressed", mk("flat", "flat"), mk("down", "up"), "revert", "score regressed vs baseline"},
		{"target test regressed", mk("flat", "flat"), mk("flat", "down"), "revert", "score regressed vs baseline"},
		{"improved on both", mk("flat", "flat"), mk("up", "up"), "keep", "score improved on train and test vs baseline"},
		{"overfit", mk("flat", "flat"), mk("up", "flat"), "revert", "overfit suspect: train improved but test is flat vs baseline"},
		{"test alone rerun", mk("flat", "flat"), mk("flat", "up"), "rerun", "test improved but train did not; add reps to v1 and baseline, then gate v1 again"},
		{"no change", mk("flat", "flat"), mk("flat", "flat"), "revert", "no change outside noise vs baseline"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			guards := []guard{{Metric: "latency_s", pair: tc.guard}}
			got, reason := decideRound(roundInput{Target: "score", Pair: tc.target, Guards: guards, Ref: "baseline", Variant: "v1", Reruns: 0})
			if got != tc.want || reason != tc.reason {
				t.Fatalf("decideRound = %q %q, want %q %q", got, reason, tc.want, tc.reason)
			}
		})
	}
}

func TestRerunLimit(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name   string
		target pair
		reruns int
		want   string
		reason string
	}{
		{"first rerun stays a rerun", mk("flat", "up"), 1, "rerun", "test improved but train did not; add reps to v1 and baseline, then gate v1 again"},
		{"third rerun reverts", mk("flat", "up"), 2, "revert", "rerun limit reached: v1 was rerun 2 times"},
		{"keep ignores the limit", mk("up", "up"), 2, "keep", "score improved on train and test vs baseline"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			guards := []guard{{Metric: "latency_s", pair: mk("flat", "flat")}}
			got, reason := decideRound(roundInput{Target: "score", Pair: tc.target, Guards: guards, Ref: "baseline", Variant: "v1", Reruns: tc.reruns})
			if got != tc.want || reason != tc.reason {
				t.Fatalf("decideRound = %q %q, want %q %q", got, reason, tc.want, tc.reason)
			}
		})
	}
}

func record(s *state, r round) {
	if n := len(s.Rounds); n > 0 && s.Rounds[n-1].Decision == "rerun" && s.Rounds[n-1].Variant == r.Variant {
		r.Round = s.Rounds[n-1].Round
		s.Rounds[n-1] = r
		return
	}
	s.Rounds = append(s.Rounds, r)
}

func TestPlateauAfterReverts(t *testing.T) {
	t.Parallel()
	s := &state{Stop: stopRule{Plateau: 3, Rounds: 10}}
	decisions := []string{"baseline", "revert", "revert", "keep", "revert", "rerun", "revert", "revert"}
	wantFlat := []int{0, 1, 2, 0, 1, 1, 2, 3}
	for i, decision := range decisions {
		variant := fmt.Sprintf("v%d", i)
		if i == 0 {
			variant = "baseline"
		}
		record(s, round{Round: i, Variant: variant, Decision: decision})
		s.advance(decision)
		if s.FlatRounds != wantFlat[i] {
			t.Fatalf("after %d decisions FlatRounds = %d, want %d", i+1, s.FlatRounds, wantFlat[i])
		}
		done, reason := s.done()
		if i < 7 && done {
			t.Fatalf("done after %d decisions with reason %q", i+1, reason)
		}
		if i == 7 && (!done || reason != "plateau after 3 reverts") {
			t.Fatalf("done = %v %q, want true plateau after 3 reverts", done, reason)
		}
	}
}

func TestRoundCeilingReplacesRerun(t *testing.T) {
	t.Parallel()
	s := &state{Stop: stopRule{Plateau: 3, Rounds: 2}}
	steps := []round{
		{Variant: "baseline", Decision: "baseline"},
		{Variant: "v1", Decision: "revert"},
		{Variant: "v2", Decision: "rerun"},
		{Variant: "v2", Decision: "keep"},
	}
	wantDone := []bool{false, false, false, true}
	for i, step := range steps {
		step.Round = i
		record(s, step)
		s.advance(step.Decision)
		done, reason := s.done()
		if done != wantDone[i] {
			t.Fatalf("step %d done = %v (%q), want %v", i, done, reason, wantDone[i])
		}
		if done && reason != "round ceiling 2 reached" {
			t.Fatalf("reason = %q, want round ceiling 2 reached", reason)
		}
	}
}
