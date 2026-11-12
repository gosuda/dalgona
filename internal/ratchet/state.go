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
	Round    int    `json:"round"`
	Variant  string `json:"variant"`
	Decision string `json:"decision"`
	Reruns   int    `json:"reruns"`
}

type approval struct {
	SHA string    `json:"sha"`
	At  time.Time `json:"at"`
}

type state struct {
	Flow         string              `json:"flow"`
	CasesPaths   []string            `json:"cases_paths"`
	HarnessPaths []string            `json:"harness_paths"`
	ChangePaths  []string            `json:"change_paths"`
	OffLimits    []string            `json:"off_limits"`
	Goal         goal                `json:"goal"`
	Reps         int                 `json:"reps"`
	Command      string              `json:"command"`
	TrainIDs     []string            `json:"train_ids"`
	TestIDs      []string            `json:"test_ids"`
	SplitSeed    uint64              `json:"split_seed"`
	TestFraction float64             `json:"test_fraction"`
	Approvals    map[string]approval `json:"approvals"`
	Stop         stopRule            `json:"stop"`
	Rounds       []round             `json:"rounds"`
	FlatRounds   int                 `json:"flat_rounds"`
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
