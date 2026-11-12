package ratchet

import "fmt"

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

type state struct {
	Stop       stopRule `json:"stop"`
	Rounds     []round  `json:"rounds"`
	FlatRounds int      `json:"flat_rounds"`
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
