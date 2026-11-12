package ratchet

import (
	"fmt"
	"strings"
)

const maxReruns = 2

type pair struct {
	Train delta `json:"train"`
	Test  delta `json:"test"`
}

type guard struct {
	Metric string
	pair
}

//nolint:revive // the six inputs of one round decision are fixed by the ratchet plan
func decideRound(target string, t pair, guards []guard, ref, variant string, reruns int) (string, string) {
	decision, reason := judgeRound(target, t, guards, ref, variant)
	if decision == "rerun" && reruns >= maxReruns {
		return "revert", fmt.Sprintf("rerun limit reached: %s was rerun %d times", variant, maxReruns)
	}
	return decision, reason
}

func judgeRound(target string, t pair, guards []guard, ref, variant string) (string, string) {
	if regressed := regressedGuards(guards); len(regressed) > 0 {
		return "revert", "guardrail regressed vs " + ref + ": " + strings.Join(regressed, ", ")
	}
	return judgeTarget(target, t, ref, variant)
}

func regressedGuards(guards []guard) []string {
	var regressed []string
	for _, g := range guards {
		if anyVerdict(g.pair, "down") {
			regressed = append(regressed, g.Metric)
		}
	}
	return regressed
}

func judgeTarget(target string, t pair, ref, variant string) (string, string) {
	switch {
	case anyVerdict(t, "down"):
		return "revert", target + " regressed vs " + ref
	case t.Train.Verdict == "up" && t.Test.Verdict == "up":
		return "keep", target + " improved on train and test vs " + ref
	case t.Train.Verdict == "up":
		return "revert", "overfit suspect: train improved but test is flat vs " + ref
	case t.Test.Verdict == "up":
		return "rerun", fmt.Sprintf("test improved but train did not; add reps to %s and %s, then gate %s again", variant, ref, variant)
	default:
		return "revert", "no change outside noise vs " + ref
	}
}

func anyVerdict(p pair, verdict string) bool {
	return p.Train.Verdict == verdict || p.Test.Verdict == verdict
}
