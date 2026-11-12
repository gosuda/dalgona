package ratchet

import (
	"cmp"
	"errors"
	"fmt"
	"maps"
	"math"
	"math/rand/v2"
	"slices"
	"strings"
)

const (
	maxHold    = 8
	directionH = "higher"
	directionL = "lower"
)

var lowerByDefault = map[string]bool{"cost_usd": true, "latency_s": true}

type initArgs struct {
	Cases     []string
	Harness   []string
	Change    []string
	OffLimits []string
}

type planArgs struct {
	Target     string
	Direction  string
	Hold       []string
	Directions map[string]string
	Reps       *int
	Plateau    *int
	Rounds     *int
	Command    string
}

func (f *flow) init(s *state, in initArgs) error {
	switch {
	case len(in.Cases) == 0:
		return errors.New("init needs at least one cases path")
	case len(in.Harness) == 0:
		return errors.New("init needs at least one harness path")
	case len(in.Change) == 0:
		return errors.New("init needs at least one change path")
	}
	cases, err := f.workspacePaths(in.Cases)
	if err != nil {
		return err
	}
	harness, err := f.workspacePaths(in.Harness)
	if err != nil {
		return err
	}
	change, err := f.workspacePaths(in.Change)
	if err != nil {
		return err
	}
	offLimits, err := repoPaths(in.OffLimits)
	if err != nil {
		return err
	}
	if err := checkOverlaps(change, slices.Concat(cases, harness, offLimits)); err != nil {
		return err
	}
	s.Flow = f.name
	s.CasesPaths = cases
	s.HarnessPaths = harness
	s.ChangePaths = change
	s.OffLimits = offLimits
	return nil
}

func checkOverlaps(change, frozen []string) error {
	for _, c := range change {
		for _, o := range frozen {
			if overlaps(c, o) {
				return fmt.Errorf("change path %s overlaps %s; cases, harness and off-limits paths must stay out of the tuned surface", c, o)
			}
		}
	}
	return nil
}

func defaultDirection(metric string) string {
	if lowerByDefault[metric] {
		return directionL
	}
	return directionH
}

func (s *state) applyPlan(p planArgs) error {
	next := *s
	if p.Target != "" {
		if err := next.setGoal(p); err != nil {
			return err
		}
	}
	if err := next.setReps(p.Reps); err != nil {
		return err
	}
	if err := next.setStop(p.Plateau, p.Rounds); err != nil {
		return err
	}
	if p.Command != "" {
		if !strings.Contains(p.Command, "{variant}") {
			return errors.New("command must contain {variant}")
		}
		next.Command = p.Command
	}
	*s = next
	return nil
}

func (s *state) setGoal(p planArgs) error {
	if len(p.Hold) > maxHold {
		return fmt.Errorf("hold takes at most %d metrics", maxHold)
	}
	if slices.Contains(p.Hold, p.Target) {
		return errors.New("hold must not include the target metric")
	}
	if p.Direction != "" && p.Direction != directionH && p.Direction != directionL {
		return fmt.Errorf("direction must be higher or lower: %s", p.Direction)
	}
	directions := make(map[string]string, len(p.Directions))
	maps.Copy(directions, p.Directions)
	s.Goal = goal{
		Target:     p.Target,
		Direction:  cmp.Or(p.Direction, defaultDirection(p.Target)),
		Hold:       slices.Clone(p.Hold),
		Directions: directions,
	}
	return nil
}

func (s *state) setReps(reps *int) error {
	switch {
	case reps != nil:
		if *reps < 1 || *reps > 20 {
			return errors.New("reps must be from 1 to 20")
		}
		if s.hasBaseline() && *reps < s.Reps {
			return errors.New("reps cannot shrink after the baseline")
		}
		s.Reps = *reps
	case s.Reps == 0:
		s.Reps = 2
	}
	return nil
}

func (s *state) setStop(plateau, rounds *int) error {
	switch {
	case plateau != nil:
		if *plateau < 2 || *plateau > 10 {
			return errors.New("plateau must be from 2 to 10")
		}
		s.Stop.Plateau = *plateau
	case s.Stop.Plateau == 0:
		s.Stop.Plateau = 3
	}
	switch {
	case rounds != nil:
		if *rounds < 1 || *rounds > 50 {
			return errors.New("rounds must be from 1 to 50")
		}
		s.Stop.Rounds = *rounds
	case s.Stop.Rounds == 0:
		s.Stop.Rounds = 10
	}
	return nil
}

func (s *state) split(strata map[string]string, fraction float64, seed uint64) error {
	if err := checkSplitArgs(s, strata, fraction); err != nil {
		return err
	}
	train, test := stratifiedSplit(strata, fraction, seed)
	train, test = balance(train, test)
	slices.Sort(train)
	slices.Sort(test)
	s.TrainIDs = train
	s.TestIDs = test
	s.SplitSeed = seed
	s.TestFraction = fraction
	return nil
}

func checkSplitArgs(s *state, strata map[string]string, fraction float64) error {
	if s.hasBaseline() {
		return errors.New("the split is frozen once the baseline is gated")
	}
	if fraction < 0.1 || fraction > 0.9 {
		return errors.New("test_fraction must be from 0.1 to 0.9")
	}
	if len(strata) < 4 {
		return errors.New("split needs at least 4 cases")
	}
	for id := range strata {
		if !caseID.MatchString(id) {
			return fmt.Errorf("case id %q must match %s", id, caseIDPattern)
		}
	}
	return nil
}

func stratifiedSplit(strata map[string]string, fraction float64, seed uint64) ([]string, []string) {
	ids := slices.Sorted(maps.Keys(strata))
	byTag := make(map[string][]string)
	for _, id := range ids {
		byTag[strata[id]] = append(byTag[strata[id]], id)
	}
	//nolint:gosec // the split is seeded so that the recorded seed reproduces it
	random := rand.New(rand.NewPCG(seed, seed^0x9E3779B97F4A7C15))
	var train, test []string
	for _, tag := range slices.Sorted(maps.Keys(byTag)) {
		bucket := byTag[tag]
		random.Shuffle(len(bucket), func(i, j int) { bucket[i], bucket[j] = bucket[j], bucket[i] })
		k := int(math.Round(float64(len(bucket)) * fraction))
		test = append(test, bucket[:k]...)
		train = append(train, bucket[k:]...)
	}
	return train, test
}

func balance(train, test []string) ([]string, []string) {
	if len(test) == 0 {
		test = append(test, train[len(train)-1])
		train = train[:len(train)-1]
	}
	if len(train) == 0 {
		train = append(train, test[len(test)-1])
		test = test[:len(test)-1]
	}
	return train, test
}
