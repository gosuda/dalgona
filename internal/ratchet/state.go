package ratchet

import (
	"fmt"
	"time"
)

type goal struct {
	Target     string            `json:"target"`
	Direction  string            `json:"direction"`
	Hold       []string          `json:"hold"`
	Directions map[string]string `json:"directions"`
}

type stopRule struct {
	Plateau int `json:"plateau"`
	Rounds  int `json:"rounds"`
}

type round struct {
	Round        int                `json:"round"`
	Variant      string             `json:"variant"`
	Change       string             `json:"change"`
	Decision     string             `json:"decision"`
	Reasons      []string           `json:"reasons"`
	Warnings     []string           `json:"warnings"`
	Train        map[string]summary `json:"train"`
	Test         map[string]summary `json:"test"`
	Deltas       map[string]pair    `json:"deltas"`
	Errors       int                `json:"errors"`
	Truncated    int                `json:"truncated"`
	Reruns       int                `json:"reruns"`
	ResultsSHA   string             `json:"results_sha"`
	ChangeSHA    string             `json:"change_sha"`
	ChangeFiles  []fileSHA          `json:"change_files"`
	ChangedFiles []string           `json:"changed_files"`
	At           time.Time          `json:"at"`
}

type approval struct {
	SHA string    `json:"sha"`
	At  time.Time `json:"at"`
}

type pending struct {
	Variant   string    `json:"variant"`
	ChangeSHA string    `json:"change_sha"`
	At        time.Time `json:"at"`
}

type bestRound struct {
	Round     int      `json:"round"`
	Variant   string   `json:"variant"`
	TestScore *float64 `json:"test_score"`
}

type state struct {
	Flow              string              `json:"flow"`
	CasesPaths        []string            `json:"cases_paths"`
	HarnessPaths      []string            `json:"harness_paths"`
	ChangePaths       []string            `json:"change_paths"`
	OffLimits         []string            `json:"off_limits"`
	Goal              goal                `json:"goal"`
	Reps              int                 `json:"reps"`
	Command           string              `json:"command"`
	TrainIDs          []string            `json:"train_ids"`
	TestIDs           []string            `json:"test_ids"`
	SplitSeed         uint64              `json:"split_seed"`
	TestFraction      float64             `json:"test_fraction"`
	Approvals         map[string]approval `json:"approvals"`
	BaselineInputsSHA string              `json:"baseline_inputs_sha,omitzero"`
	OffLimitsSHA      string              `json:"off_limits_sha,omitzero"`
	Pending           *pending            `json:"pending,omitzero"`
	Best              *bestRound          `json:"best,omitzero"`
	Stop              stopRule            `json:"stop"`
	Rounds            []round             `json:"rounds"`
	FlatRounds        int                 `json:"flat_rounds"`
}

func (s *state) advance(decision string) {
	switch decision {
	case "keep", "baseline":
		s.FlatRounds = 0
	case "revert":
		s.FlatRounds++
	}
}

func (s *state) planned() bool {
	return s.Stop.Plateau > 0
}

func (s *state) plateauReached() bool {
	return s.planned() && s.FlatRounds >= s.Stop.Plateau
}

func (s *state) done() (bool, string) {
	if !s.planned() {
		return false, ""
	}
	if n := len(s.Rounds); n > 0 && s.Rounds[n-1].Decision == "rerun" {
		return false, ""
	}
	if s.plateauReached() {
		return true, fmt.Sprintf("plateau after %d reverts", s.FlatRounds)
	}
	if s.gatedRounds() >= s.Stop.Rounds {
		return true, fmt.Sprintf("round ceiling %d reached", s.Stop.Rounds)
	}
	return false, ""
}

func (s *state) gatedRounds() int {
	count := 0
	for _, r := range s.Rounds {
		if r.Variant != "baseline" {
			count++
		}
	}
	return count
}

func (s *state) hasBaseline() bool {
	for _, r := range s.Rounds {
		if r.Variant == "baseline" {
			return true
		}
	}
	return false
}

func (s *state) lastRound() (round, bool) {
	if len(s.Rounds) == 0 {
		return round{}, false
	}
	return s.Rounds[len(s.Rounds)-1], true
}

func (s *state) round(n int) (round, bool) {
	for _, r := range s.Rounds {
		if r.Round == n {
			return r, true
		}
	}
	return round{}, false
}

func (s *state) gated(variant string) bool {
	for _, r := range s.Rounds {
		if r.Variant == variant {
			return true
		}
	}
	return false
}
