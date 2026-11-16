package script

import (
	"context"
	"encoding/json/jsontext"
	"testing"
	"time"
)

type stubEffects struct{}

func (stubEffects) Started(start RunStart) (RunStart, *Outcome, error) { return start, nil, nil }
func (stubEffects) Lookup(string, int) (Recorded, bool)                { return Recorded{}, false }
func (stubEffects) Record(Slot, jsontext.Value) error                  { return nil }
func (stubEffects) Completed(string, Outcome) error                    { return nil }
func (stubEffects) CallTool(context.Context, Slot, string, jsontext.Value) (jsontext.Value, error) {
	return jsontext.Value(`"ok"`), nil
}

func (stubEffects) Spawn(context.Context, Slot, string, SpawnOpts) (string, error) {
	return "child", nil
}
func (stubEffects) Wait(context.Context, []string) ([]string, error) { return nil, nil }
func (stubEffects) Judge(context.Context, Slot, JudgeRequest) (jsontext.Value, error) {
	return jsontext.Value(`{}`), nil
}
func (stubEffects) Log(string, string) {}

func TestJSCellEvalBudgetStopsRunawayLoop(t *testing.T) {
	old := jsEvalBudget
	jsEvalBudget = 50 * time.Millisecond
	defer func() { jsEvalBudget = old }()
	r := newRun(stubEffects{}, "run-1", runConfig{})
	out := r.executeJS(t.Context(), jsontext.Value(`while(true){}`), nil, 1)
	if out.Status != StatusLimitExceeded || out.Error.Code != codeLimit {
		t.Fatalf("outcome = %+v; a runaway loop must stop as limit_exceeded", out)
	}
}
