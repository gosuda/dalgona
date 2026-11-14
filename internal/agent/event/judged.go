package event

import (
	"errors"
	"fmt"
	"maps"
	"math"
	"slices"
	"strings"
)

const (
	MaxJudgedPaths     = 64
	MaxJudgeErrorBytes = 1 << 10
)

type TurnJudged struct {
	TurnID    string
	By        string
	Verdict   string
	Paths     []string
	Scores    map[string]float64
	Flags     []string
	Spoken    []string
	Skipped   []string
	Feedback  bool
	Truncated bool
	Error     string
	ElapsedMS int64
}

func (TurnJudged) Type() string { return TypeTurnJudged }

func (e TurnJudged) Validate() error {
	if err := required(map[string]string{"turn_id": e.TurnID, "by": e.By}); err != nil {
		return err
	}
	if err := e.shared().validate(); err != nil {
		return err
	}
	if err := checkNames("scores", slices.Sorted(maps.Keys(e.Scores)), "sound", "incomplete"); err != nil {
		return err
	}
	if err := checkNames("flags", e.Flags, "incomplete"); err != nil {
		return err
	}
	return validateSkipped(e.Skipped, e.Paths)
}

func (e TurnJudged) shared() judgment {
	return judgment{
		Verdict: e.Verdict, Paths: e.Paths, Scores: e.Scores, Flags: e.Flags, Spoken: e.Spoken,
		Feedback: e.Feedback, Error: e.Error, ElapsedMS: e.ElapsedMS,
	}
}

func checkNames(field string, names []string, allowed ...string) error {
	for _, name := range names {
		if !slices.Contains(allowed, name) {
			return fmt.Errorf("%s %q is not a turn question; use %s", field, name, strings.Join(allowed, " or "))
		}
	}
	return nil
}

type ToolJudged struct {
	CallID      string
	TurnID      string
	By          string
	Verdict     string
	Paths       []string
	Scores      map[string]float64
	Flags       []string
	Spoken      []string
	Feedback    bool
	Truncated   bool
	Parts       int
	FailedParts int
	Error       string
	ElapsedMS   int64
}

func (ToolJudged) Type() string { return TypeToolJudged }

func (e ToolJudged) Validate() error {
	if err := required(map[string]string{"call_id": e.CallID, "turn_id": e.TurnID, "by": e.By}); err != nil {
		return err
	}
	if e.Parts < 1 {
		return errors.New("tool.judged parts must be 1 or more")
	}
	if e.FailedParts < 0 || e.FailedParts > e.Parts {
		return errors.New("tool.judged failed_parts must be from 0 to parts")
	}
	return e.shared().validate()
}

func (e ToolJudged) shared() judgment {
	return judgment{
		Verdict: e.Verdict, Paths: e.Paths, Scores: e.Scores, Flags: e.Flags, Spoken: e.Spoken,
		Feedback: e.Feedback, Error: e.Error, ElapsedMS: e.ElapsedMS,
	}
}

type judgment struct {
	Verdict   string
	Paths     []string
	Scores    map[string]float64
	Flags     []string
	Spoken    []string
	Feedback  bool
	Error     string
	ElapsedMS int64
}

func (j judgment) validate() error {
	if err := oneOf("verdict", j.Verdict, "clean", "uncertain", "flagged", "error"); err != nil {
		return err
	}
	if err := validatePaths(j.Paths); err != nil {
		return err
	}
	if j.ElapsedMS < 0 {
		return errors.New("elapsed_ms must not be negative")
	}
	if j.Verdict == "error" {
		return j.validateError()
	}
	return j.validateAnswer()
}

func (j judgment) validateError() error {
	if j.Error == "" || len(j.Error) > MaxJudgeErrorBytes {
		return fmt.Errorf("an error verdict needs an error of 1 to %d bytes", MaxJudgeErrorBytes)
	}
	if len(j.Scores) > 0 || len(j.Flags) > 0 || len(j.Spoken) > 0 || j.Feedback {
		return errors.New("an error verdict must not carry scores, flags, or feedback")
	}
	return nil
}

func (j judgment) validateAnswer() error {
	if j.Error != "" {
		return errors.New("only an error verdict may carry an error")
	}
	if len(j.Scores) == 0 {
		return errors.New("this verdict needs scores")
	}
	if err := validateScores(j.Scores); err != nil {
		return err
	}
	if err := checkFlags(j.Verdict, j.Flags, j.Scores); err != nil {
		return err
	}
	return checkSpoken(j.Flags, j.Spoken, j.Feedback)
}

func checkFlags(verdict string, flags []string, scores map[string]float64) error {
	if (verdict == "flagged") != (len(flags) > 0) {
		return errors.New("only a flagged verdict has flags, and it needs one or more")
	}
	if err := noRepeats("flags", flags); err != nil {
		return err
	}
	for _, flag := range flags {
		if _, ok := scores[flag]; !ok {
			return fmt.Errorf("flag %s is not a scored question", flag)
		}
	}
	return nil
}

func checkSpoken(flags, spoken []string, feedback bool) error {
	if err := noRepeats("spoken", spoken); err != nil {
		return err
	}
	for _, name := range spoken {
		if !slices.Contains(flags, name) {
			return fmt.Errorf("spoken %s is not a flag", name)
		}
	}
	if feedback != (len(spoken) > 0) {
		return errors.New("feedback must be true exactly when spoken is not empty")
	}
	return nil
}

func validatePaths(paths []string) error {
	if len(paths) < 1 || len(paths) > MaxJudgedPaths {
		return fmt.Errorf("paths needs 1 to %d entries", MaxJudgedPaths)
	}
	for i, path := range paths {
		if path == "" {
			return fmt.Errorf("paths[%d] must not be empty", i)
		}
	}
	return nil
}

func validateSkipped(skipped, paths []string) error {
	if err := noRepeats("skipped", skipped); err != nil {
		return err
	}
	for _, path := range skipped {
		if !slices.Contains(paths, path) {
			return fmt.Errorf("skipped %s is not a judged path", path)
		}
	}
	return nil
}

func noRepeats(name string, values []string) error {
	seen := make(map[string]bool, len(values))
	for _, value := range values {
		if seen[value] {
			return fmt.Errorf("%s repeats %s", name, value)
		}
		seen[value] = true
	}
	return nil
}

func validateScores(scores map[string]float64) error {
	for name, score := range scores {
		if name == "" || math.IsNaN(score) || math.IsInf(score, 0) || score < 0 || score > 1 {
			return errors.New("scores must have named probabilities between zero and one")
		}
	}
	return nil
}
