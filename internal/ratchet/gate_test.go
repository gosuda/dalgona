package ratchet

import (
	"bytes"
	"fmt"
	"maps"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"
)

var fixtureIDs = []string{"a", "b", "c", "d", "e", "f", "g", "h"}

type fixture struct {
	t *testing.T
	f *flow
	s *state
}

func newFixture(t *testing.T, hold ...string) *fixture {
	t.Helper()
	f := newWorkspace(t, map[string]string{"cases.jsonl": "x\n", "run.py": "y\n", "p.md": "one\n", "notes.md": "n\n"})
	var s state
	in := initArgs{Cases: []string{"cases.jsonl"}, Harness: []string{"run.py"}, Change: []string{"p.md"}, OffLimits: []string{"notes.md"}}
	if err := f.init(&s, in); err != nil {
		t.Fatal(err)
	}
	s.TrainIDs = []string{"a", "b", "c", "d"}
	s.TestIDs = []string{"e", "f", "g", "h"}
	s.SplitSeed = 7
	s.TestFraction = 0.5
	plan := planArgs{Target: "score", Hold: hold, Command: "run {variant} --out {flow_dir}/{variant}", Reps: new(1)}
	if err := s.applyPlan(plan); err != nil {
		t.Fatal(err)
	}
	fx := &fixture{t: t, f: f, s: &s}
	fx.approve()
	return fx
}

func (fx *fixture) approve() {
	fx.t.Helper()
	fx.s.Approvals = map[string]approval{}
	for _, stage := range stages {
		sum, err := fx.f.digest(fx.s, stage)
		if err != nil {
			fx.t.Fatal(err)
		}
		fx.s.Approvals[stage] = approval{SHA: sum, At: time.Now().UTC()}
	}
}

func (fx *fixture) writeFile(rel, body string) {
	fx.t.Helper()
	if err := fx.f.root.WriteFile(rel, []byte(body), 0o600); err != nil {
		fx.t.Fatal(err)
	}
}

func (fx *fixture) writeArtifact(variant, name string, lines ...string) {
	fx.t.Helper()
	dir := filepath.Join(fx.f.dir, variant)
	if err := fx.f.root.MkdirAll(dir, 0o750); err != nil {
		fx.t.Fatal(err)
	}
	body := strings.Join(lines, "\n") + "\n"
	if err := fx.f.root.WriteFile(filepath.Join(dir, name), []byte(body), 0o600); err != nil {
		fx.t.Fatal(err)
	}
}

func (fx *fixture) writeResults(variant string, lines ...string) {
	fx.t.Helper()
	fx.writeArtifact(variant, "results.jsonl", lines...)
}

func (fx *fixture) writeTrace(variant, name string) {
	fx.t.Helper()
	dir := filepath.Join(fx.f.dir, variant, "traces")
	if err := fx.f.root.MkdirAll(dir, 0o750); err != nil {
		fx.t.Fatal(err)
	}
	if err := fx.f.root.WriteFile(filepath.Join(dir, name), []byte("{}\n"), 0o600); err != nil {
		fx.t.Fatal(err)
	}
}

func (fx *fixture) check(variant string) {
	fx.t.Helper()
	if _, err := fx.f.check(fx.s, variant); err != nil {
		fx.t.Fatal(err)
	}
}

func (fx *fixture) gate(variant string) (gateResult, error) {
	return fx.f.gate(fx.s, variant, variant)
}

func (fx *fixture) mustGate(variant string) gateResult {
	fx.t.Helper()
	result, err := fx.gate(variant)
	if err != nil {
		fx.t.Fatal(err)
	}
	return result
}

func (fx *fixture) baseline(lines []string) gateResult {
	fx.t.Helper()
	fx.check("baseline")
	fx.writeResults("baseline", lines...)
	return fx.mustGate("baseline")
}

func (fx *fixture) changeP(body string) {
	fx.t.Helper()
	fx.writeFile("p.md", body)
}

func (fx *fixture) keepV1() {
	fx.t.Helper()
	fx.baseline(rowsFor(0, allGrade(false), noExtra))
	fx.changeP("two\n")
	fx.check("v1")
	fx.writeResults("v1", rowsFor(0, allGrade(true), noExtra)...)
	if result := fx.mustGate("v1"); result.Decision != "keep" {
		fx.t.Fatalf("v1 decision = %s, want keep", result.Decision)
	}
}

func (fx *fixture) rerunV1() {
	fx.t.Helper()
	fx.baseline(rowsFor(0, allGrade(false), noExtra))
	fx.changeP("two\n")
	fx.check("v1")
	fx.writeResults("v1", rowsFor(0, rerunGrade, noExtra)...)
	if result := fx.mustGate("v1"); result.Decision != "rerun" {
		fx.t.Fatalf("v1 decision = %s, want rerun", result.Decision)
	}
}

func rowLine(id string, rep int, grade bool, extra string) string {
	return fmt.Sprintf(`{"case_id":%q,"rep":%d,"status":"ok","grade":%t%s}`, id, rep, grade, extra)
}

func rowsFor(rep int, grade func(id string) bool, extra func(id string) string) []string {
	lines := make([]string, 0, len(fixtureIDs))
	for _, id := range fixtureIDs {
		lines = append(lines, rowLine(id, rep, grade(id), extra(id)))
	}
	return lines
}

func gradeRows(rep int, grade string) []string {
	lines := make([]string, 0, len(fixtureIDs))
	for _, id := range fixtureIDs {
		lines = append(lines, fmt.Sprintf(`{"case_id":%q,"rep":%d,"status":"ok","grade":%s}`, id, rep, grade))
	}
	return lines
}

func gateWithFirstRow(fx *fixture, first string) error {
	fx.baseline(rowsFor(0, allGrade(false), noExtra))
	fx.changeP("two\n")
	fx.check("v1")
	rows := rowsFor(0, allGrade(true), noExtra)
	rows[0] = first
	fx.writeResults("v1", rows...)
	before, err := fx.f.root.ReadFile(filepath.Join(fx.f.dir, "state.json"))
	if err != nil {
		fx.t.Fatal(err)
	}
	_, err = fx.gate("v1")
	after, readErr := fx.f.root.ReadFile(filepath.Join(fx.f.dir, "state.json"))
	if readErr != nil {
		fx.t.Fatal(readErr)
	}
	if !bytes.Equal(before, after) {
		fx.t.Fatal("refused gate changed state.json")
	}
	return err
}

func noExtra(string) string { return "" }

func allGrade(v bool) func(string) bool {
	return func(string) bool { return v }
}

func rerunGrade(id string) bool { return id != "d" }

func trainTrue(id string) bool { return id < "e" }

func TestGateTruncatedGradeExcludesQualityAndKeepsRuntimeMetrics(t *testing.T) {
	t.Parallel()
	for _, grade := range []string{`false`, `0`, `{"score":false,"latency_s":99}`} {
		t.Run(grade, func(t *testing.T) {
			t.Parallel()
			fx := newFixture(t, "latency_s")
			extra := func(string) string { return `,"latency_s":1` }
			fx.baseline(rowsFor(0, allGrade(false), extra))
			fx.changeP("two\n")
			fx.check("v1")
			rows := rowsFor(0, allGrade(true), extra)
			rows[0] = fmt.Sprintf(`{"case_id":"a","rep":0,"status":"truncated","grade":%s,"latency_s":1}`, grade)
			fx.writeResults("v1", rows...)
			result := fx.mustGate("v1")
			if score := result.Train["score"]; score.N != 3 || score.Mean == nil || *score.Mean != 1 {
				t.Fatalf("train score = %+v, want 3 cases with mean 1", score)
			}
			if latency := result.Train["latency_s"]; latency.N != 4 || latency.Mean == nil || *latency.Mean != 1 {
				t.Fatalf("train latency = %+v, want 4 cases with mean 1", latency)
			}
			if !slices.Contains(result.Warnings, "1 truncated rows excluded from quality means") {
				t.Fatalf("warnings = %v, want truncated quality exclusion", result.Warnings)
			}
		})
	}
}

func TestGateTruncatedNumericGradeExcludesGradeMetric(t *testing.T) {
	t.Parallel()
	fx := newFixture(t, "grade", "latency_s")
	baseline := make([]string, len(fixtureIDs))
	rows := make([]string, len(fixtureIDs))
	for i, id := range fixtureIDs {
		baseline[i] = fmt.Sprintf(`{"case_id":%q,"rep":0,"status":"ok","grade":0,"latency_s":1}`, id)
		rows[i] = fmt.Sprintf(`{"case_id":%q,"rep":0,"status":"ok","grade":1,"latency_s":1}`, id)
	}
	fx.baseline(baseline)
	fx.changeP("two\n")
	fx.check("v1")
	rows[0] = `{"case_id":"a","rep":0,"status":"truncated","grade":0,"latency_s":1}`
	fx.writeResults("v1", rows...)
	result := fx.mustGate("v1")
	for _, check := range []struct {
		name string
		got  summary
		want int
	}{
		{"train grade", result.Train["grade"], 3},
		{"test grade", result.Test["grade"], 4},
		{"train latency", result.Train["latency_s"], 4},
	} {
		if check.got.N != check.want || check.got.Mean == nil || *check.got.Mean != 1 {
			t.Fatalf("%s = %+v, want %d cases with mean 1", check.name, check.got, check.want)
		}
	}
}

func TestGateClimb(t *testing.T) {
	t.Parallel()
	fx := newFixture(t)
	climbBaseline(t, fx)
	climbRerunAndKeep(t, fx)
	climbRegressionReverts(t, fx)
}

func climbBaseline(t *testing.T, fx *fixture) {
	t.Helper()
	base := fx.baseline(rowsFor(0, allGrade(false), noExtra))
	if base.Decision != "baseline" || len(base.Warnings) != 0 {
		t.Fatalf("baseline = %s with warnings %v, want baseline and none", base.Decision, base.Warnings)
	}
	if train := base.Train["score"]; train.N != 4 || *train.Mean != 0 || *train.Half != 0 {
		t.Fatalf("baseline train score = %+v, want n 4, mean 0, half 0", train)
	}
}

func climbRerunAndKeep(t *testing.T, fx *fixture) {
	t.Helper()
	fx.changeP("two\n")
	fx.check("v1")
	fx.writeResults("v1", rowsFor(0, rerunGrade, noExtra)...)
	if r := fx.mustGate("v1"); r.Decision != "rerun" || fx.s.Rounds[len(fx.s.Rounds)-1].Reruns != 1 || fx.s.FlatRounds != 0 {
		t.Fatalf("v1 first gate = %s, want rerun with one rerun and no flat rounds", r.Decision)
	}
	fx.writeResults("baseline", append(rowsFor(0, allGrade(false), noExtra), rowsFor(1, allGrade(false), noExtra)...)...)
	fx.writeResults("v1", append(rowsFor(0, rerunGrade, noExtra), rowsFor(1, allGrade(true), noExtra)...)...)
	if r := fx.mustGate("v1"); r.Decision != "keep" || r.Best == nil || r.Best.Variant != "v1" {
		t.Fatalf("v1 with a second rep = %s, want keep as best v1", r.Decision)
	}
}

func climbRegressionReverts(t *testing.T, fx *fixture) {
	t.Helper()
	fx.changeP("three\n")
	fx.check("v2")
	fx.writeResults("v2", append(rowsFor(0, trainTrue, noExtra), rowsFor(1, trainTrue, noExtra)...)...)
	r := fx.mustGate("v2")
	if r.Decision != "revert" || r.Reasons[0] != "score regressed vs v1" || fx.s.FlatRounds != 1 {
		t.Fatalf("v2 = %s %v with flat %d, want revert for regression", r.Decision, r.Reasons, fx.s.FlatRounds)
	}
	if !slices.Equal(r.ChangedFiles, []string{"p.md"}) {
		t.Fatalf("v2 changed files = %v, want [p.md]", r.ChangedFiles)
	}
}

func TestGateGuardrail(t *testing.T) {
	t.Parallel()
	fx := newFixture(t, "latency_s")
	latency := map[string]string{"a": "1.2", "b": "1.2", "c": "1.3", "d": "1.3", "e": "1.0", "f": "1.0", "g": "1.0", "h": "1.0"}
	extra := func(id string) string { return fmt.Sprintf(`,"latency_s":%s`, latency[id]) }
	fx.baseline(rowsFor(0, allGrade(false), func(string) string { return `,"latency_s":1.0` }))
	fx.changeP("two\n")
	fx.check("v1")
	fx.writeResults("v1", rowsFor(0, allGrade(true), extra)...)
	r := fx.mustGate("v1")
	if r.Decision != "revert" || r.Reasons[0] != "guardrail regressed vs baseline: latency_s" {
		t.Fatalf("v1 = %s %v, want a guardrail revert", r.Decision, r.Reasons)
	}
}

func TestGateErroredOnlyReruns(t *testing.T) {
	t.Parallel()
	fx := newFixture(t)
	fx.baseline(rowsFor(0, allGrade(false), noExtra))
	fx.changeP("two\n")
	fx.check("v1")
	fx.writeResults("v1", rowsFor(0, allGrade(true), noExtra)[:7]...)
	fx.writeArtifact("v1", "errors.jsonl", `{"case_id":"h","rep":0,"class":"timeout"}`)
	r := fx.mustGate("v1")
	want := "1 slots only errored (h#0); resume v1 until every slot is scored, then gate it again"
	if r.Decision != "rerun" || r.Reasons[0] != want {
		t.Fatalf("v1 = %s %v, want rerun %q", r.Decision, r.Reasons, want)
	}
}

func TestGateErroredOnlyRerunLimit(t *testing.T) {
	t.Parallel()
	fx := newFixture(t)
	fx.baseline(rowsFor(0, allGrade(false), noExtra))
	fx.changeP("two\n")
	fx.check("v1")
	fx.writeResults("v1", rowsFor(0, allGrade(true), noExtra)[:7]...)
	for i, want := range []string{"rerun", "rerun", "revert"} {
		fx.writeArtifact("v1", "errors.jsonl", fmt.Sprintf(`{"case_id":"h","rep":0,"class":"timeout","attempt":%d}`, i))
		r := fx.mustGate("v1")
		if r.Decision != want {
			t.Fatalf("gate %d = %s %v, want %s", i+1, r.Decision, r.Reasons, want)
		}
		if want == "revert" && r.Reasons[0] != "rerun limit reached: v1 was rerun 2 times" {
			t.Fatalf("third gate reason = %q, want the rerun limit reason", r.Reasons[0])
		}
	}
}

func TestGateRerunLimit(t *testing.T) {
	t.Parallel()
	fx := newFixture(t)
	fx.baseline(rowsFor(0, allGrade(false), noExtra))
	fx.changeP("two\n")
	fx.check("v1")
	fx.writeResults("v1", rowsFor(0, rerunGrade, noExtra)...)
	if r := fx.mustGate("v1"); r.Decision != "rerun" {
		t.Fatalf("first gate = %s, want rerun", r.Decision)
	}
	fx.writeResults("v1", append(rowsFor(0, rerunGrade, noExtra), rowsFor(1, rerunGrade, noExtra)...)...)
	if r := fx.mustGate("v1"); r.Decision != "rerun" {
		t.Fatalf("second gate = %s, want rerun", r.Decision)
	}
	fx.writeResults("v1", append(append(rowsFor(0, rerunGrade, noExtra), rowsFor(1, rerunGrade, noExtra)...), rowsFor(2, rerunGrade, noExtra)...)...)
	r := fx.mustGate("v1")
	if r.Decision != "revert" || r.Reasons[0] != "rerun limit reached: v1 was rerun 2 times" {
		t.Fatalf("third gate = %s %v, want the rerun limit revert", r.Decision, r.Reasons)
	}
}

func TestGateReplay(t *testing.T) {
	t.Parallel()
	fx := newFixture(t)
	fx.rerunV1()
	rounds := len(fx.s.Rounds)
	before := fx.s.Rounds[rounds-1]
	flat := fx.s.FlatRounds
	again := fx.mustGate("v1")
	after := fx.s.Rounds[len(fx.s.Rounds)-1]
	if again.Decision != "rerun" || len(fx.s.Rounds) != rounds || fx.s.FlatRounds != flat || after.Reruns != before.Reruns || !after.At.Equal(before.At) {
		t.Fatalf("replayed gate = %s with %d rounds, reruns %d, want the stored rerun round with %d rounds and reruns %d", again.Decision, len(fx.s.Rounds), after.Reruns, rounds, before.Reruns)
	}
}

func TestGateRefusals(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		run  func(fx *fixture) error
		want string
	}{
		{"incomplete", func(fx *fixture) error {
			fx.baseline(rowsFor(0, allGrade(false), noExtra))
			fx.changeP("two\n")
			fx.check("v1")
			fx.writeResults("v1", rowsFor(0, allGrade(true), noExtra)[:7]...)
			_, err := fx.gate("v1")
			return err
		}, "v1 is incomplete: 1 (case, rep) slots have no result or error row (h#0)"},
		{"duplicate", func(fx *fixture) error {
			fx.baseline(rowsFor(0, allGrade(false), noExtra))
			fx.changeP("two\n")
			fx.check("v1")
			fx.writeResults("v1", append(rowsFor(0, allGrade(true), noExtra), rowLine("a", 0, true, ""))...)
			_, err := fx.gate("v1")
			return err
		}, "v1/results.jsonl has duplicate (case, rep) rows (a#0); keep one row per slot and gate again"},
		{"transcript of a test case", func(fx *fixture) error {
			fx.baseline(rowsFor(0, allGrade(false), noExtra))
			fx.changeP("two\n")
			fx.check("v1")
			fx.writeResults("v1", rowsFor(0, allGrade(true), noExtra)...)
			fx.writeTrace("v1", "e_rep0.json")
			_, err := fx.gate("v1")
			return err
		}, "v1/traces holds transcripts for test cases (e_rep0.json); write traces for train cases only, delete these, and run again"},
		{"stray trace file", func(fx *fixture) error {
			fx.baseline(rowsFor(0, allGrade(false), noExtra))
			fx.changeP("two\n")
			fx.check("v1")
			fx.writeResults("v1", rowsFor(0, allGrade(true), noExtra)...)
			fx.writeTrace("v1", "notes.txt")
			_, err := fx.gate("v1")
			return err
		}, "v1/traces holds a file that is not <case>_rep<k>.json: notes.txt"},
		{"test row with text", func(fx *fixture) error {
			fx.baseline(rowsFor(0, allGrade(false), noExtra))
			fx.changeP("two\n")
			fx.check("v1")
			rows := rowsFor(0, allGrade(true), func(id string) string {
				if id == "e" {
					return `,"explanation":"x"`
				}
				return ""
			})
			fx.writeResults("v1", rows...)
			_, err := fx.gate("v1")
			return err
		}, "test row e#0 carries text field explanation; keep output text of test cases out of results.jsonl"},
		{"no score field", func(fx *fixture) error {
			fx.baseline(rowsFor(0, allGrade(false), noExtra))
			fx.changeP("two\n")
			fx.check("v1")
			fx.writeResults("v1", gradeRows(0, `{"pass":true}`)...)
			_, err := fx.gate("v1")
			return err
		}, `no train row carries the target metric "score"`},
		{"no test row carries the target", func(fx *fixture) error {
			fx.baseline(rowsFor(0, allGrade(false), noExtra))
			fx.changeP("two\n")
			fx.check("v1")
			fx.writeResults("v1", append(rowsFor(0, allGrade(true), noExtra)[:4], gradeRows(0, `{"pass":true}`)[4:]...)...)
			_, err := fx.gate("v1")
			return err
		}, `no test row carries the target metric "score"`},
		{"row without status", func(fx *fixture) error {
			return gateWithFirstRow(fx, `{"case_id":"a","rep":0,"grade":true}`)
		}, "v1/results.jsonl line 1: status is missing; it must be ok or truncated"},
		{"row without grade", func(fx *fixture) error {
			return gateWithFirstRow(fx, `{"case_id":"a","rep":0,"status":"ok"}`)
		}, "v1/results.jsonl line 1: grade is missing; it must be a number, a boolean, or an object of numbers and booleans"},
		{"grade is text", func(fx *fixture) error {
			return gateWithFirstRow(fx, `{"case_id":"a","rep":0,"status":"ok","grade":"pass"}`)
		}, "v1/results.jsonl line 1: grade must be a number, a boolean, or an object of numbers and booleans"},
		{"grade is null", func(fx *fixture) error {
			return gateWithFirstRow(fx, `{"case_id":"a","rep":0,"status":"ok","grade":null}`)
		}, "v1/results.jsonl line 1: grade must be a number, a boolean, or an object of numbers and booleans"},
		{"grade member is text", func(fx *fixture) error {
			return gateWithFirstRow(fx, `{"case_id":"a","rep":0,"status":"ok","grade":{"pass":"yes"}}`)
		}, "v1/results.jsonl line 1: grade member pass must be a number or a boolean"},
		{"truncated row without grade", func(fx *fixture) error {
			return gateWithFirstRow(fx, `{"case_id":"a","rep":0,"status":"truncated"}`)
		}, "v1/results.jsonl line 1: grade is missing; it must be a number, a boolean, or an object of numbers and booleans"},
		{"truncated grade is text", func(fx *fixture) error {
			return gateWithFirstRow(fx, `{"case_id":"a","rep":0,"status":"truncated","grade":"pass"}`)
		}, "v1/results.jsonl line 1: grade must be a number, a boolean, or an object of numbers and booleans"},
		{"truncated grade is null", func(fx *fixture) error {
			return gateWithFirstRow(fx, `{"case_id":"a","rep":0,"status":"truncated","grade":null}`)
		}, "v1/results.jsonl line 1: grade must be a number, a boolean, or an object of numbers and booleans"},
		{"truncated grade member is text", func(fx *fixture) error {
			return gateWithFirstRow(fx, `{"case_id":"a","rep":0,"status":"truncated","grade":{"pass":"yes"}}`)
		}, "v1/results.jsonl line 1: grade member pass must be a number or a boolean"},
		{"change edited after check", func(fx *fixture) error {
			fx.baseline(rowsFor(0, allGrade(false), noExtra))
			fx.changeP("two\n")
			fx.check("v1")
			fx.changeP("three\n")
			fx.writeResults("v1", rowsFor(0, allGrade(true), noExtra)...)
			_, err := fx.gate("v1")
			return err
		}, "the change paths changed after check(v1); call check again and run v1 again"},
		{"gate without check", func(fx *fixture) error {
			fx.baseline(rowsFor(0, allGrade(false), noExtra))
			fx.writeResults("v1", rowsFor(0, allGrade(true), noExtra)...)
			_, err := fx.gate("v1")
			return err
		}, "call check(v1) before gate(v1)"},
		{"row without rep", func(fx *fixture) error {
			fx.baseline(rowsFor(0, allGrade(false), noExtra))
			fx.changeP("two\n")
			fx.check("v1")
			fx.writeResults("v1", `{"case_id":"a","grade":true}`)
			_, err := fx.gate("v1")
			return err
		}, "v1/results.jsonl line 1: rep must be a non-negative integer"},
		{"row with unknown status", func(fx *fixture) error {
			fx.baseline(rowsFor(0, allGrade(false), noExtra))
			fx.changeP("two\n")
			fx.check("v1")
			fx.writeResults("v1", `{"case_id":"a","rep":0,"status":"done","grade":true}`)
			_, err := fx.gate("v1")
			return err
		}, "v1/results.jsonl line 1: status must be ok or truncated"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			fx := newFixture(t)
			err := tc.run(fx)
			if err == nil || err.Error() != tc.want {
				t.Fatalf("gate error = %v, want %q", err, tc.want)
			}
		})
	}
}

func TestGateBaselineErroredOnlyRefused(t *testing.T) {
	t.Parallel()
	fx := newFixture(t)
	fx.check("baseline")
	fx.writeResults("baseline", rowsFor(0, allGrade(false), noExtra)[1:]...)
	fx.writeArtifact("baseline", "errors.jsonl", `{"case_id":"a","rep":0,"class":"timeout"}`)
	_, err := fx.gate("baseline")
	want := "baseline has 1 slots that only errored (a#0); fix the harness and resume the run until every slot is scored"
	if err == nil || err.Error() != want {
		t.Fatalf("baseline gate error = %v, want %q", err, want)
	}
}

func TestCheckRefusals(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		run  func(fx *fixture) error
		want string
	}{
		{"stale grader", func(fx *fixture) error {
			fx.writeFile("run.py", "changed\n")
			_, err := fx.f.check(fx.s, "baseline")
			return err
		}, "approvals are not current (grader: stale); call approve for each stage"},
		{"skipped variant", func(fx *fixture) error {
			fx.keepV1()
			_, err := fx.f.check(fx.s, "v3")
			return err
		}, "next variant is v2, got v3"},
		{"no change since best", func(fx *fixture) error {
			fx.keepV1()
			_, err := fx.f.check(fx.s, "v2")
			return err
		}, "v2 changes nothing in the change paths since the best round (v1)"},
		{"cases edited after baseline", func(fx *fixture) error {
			fx.baseline(rowsFor(0, allGrade(false), noExtra))
			fx.writeFile("cases.jsonl", "changed\n")
			fx.approve()
			_, err := fx.f.check(fx.s, "v1")
			return err
		}, "the cases changed after the baseline was gated; start a new flow with init under a new name"},
		{"off-limits edited after baseline", func(fx *fixture) error {
			fx.baseline(rowsFor(0, allGrade(false), noExtra))
			fx.writeFile("notes.md", "changed\n")
			_, err := fx.f.check(fx.s, "v1")
			return err
		}, "off-limits files changed since the baseline; restore them before the next round"},
		{"plateau reached", func(fx *fixture) error {
			fx.baseline(rowsFor(0, allGrade(false), noExtra))
			fx.s.FlatRounds = fx.s.Stop.Plateau
			_, err := fx.f.check(fx.s, "v1")
			return err
		}, "flow is done: plateau after 3 reverts; change stop with plan and approve the plan again, or start a new flow"},
		{"next variant while one awaits rerun", func(fx *fixture) error {
			fx.rerunV1()
			_, err := fx.f.check(fx.s, "v2")
			return err
		}, "next variant is v1, got v2"},
		{"rerun with a changed change file", func(fx *fixture) error {
			fx.rerunV1()
			fx.changeP("three\n")
			_, err := fx.f.check(fx.s, "v1")
			return err
		}, "v1 awaits rerun; restore the change paths to the state gated in the last round"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			fx := newFixture(t)
			err := tc.run(fx)
			if err == nil || err.Error() != tc.want {
				t.Fatalf("check error = %v, want %q", err, tc.want)
			}
		})
	}
}

func TestTrainHidesTest(t *testing.T) {
	t.Parallel()
	fx := newFixture(t)
	fx.keepV1()
	view, err := fx.f.train(fx.s, "v1")
	if err != nil {
		t.Fatal(err)
	}
	if len(view.Rows) != len(fx.s.TrainIDs) {
		t.Fatalf("train rows = %d, want %d", len(view.Rows), len(fx.s.TrainIDs))
	}
	if !slices.Equal(slices.Sorted(maps.Keys(view.History)), fx.s.TrainIDs) {
		t.Fatalf("history cases = %v, want the train ids", slices.Sorted(maps.Keys(view.History)))
	}
	for _, name := range view.Traces {
		if strings.Contains(name, "/e_") || strings.Contains(name, "/f_") {
			t.Fatalf("trace %s names a test case", name)
		}
	}
	if _, err := fx.f.train(fx.s, "v9"); err == nil || err.Error() != "train needs a gated round for v9" {
		t.Fatalf("train of an ungated variant = %v, want the gated round refusal", err)
	}
}

func TestRenderTableMarksBest(t *testing.T) {
	t.Parallel()
	fx := newFixture(t)
	fx.keepV1()
	table := renderTable(fx.s)
	if !strings.Contains(table, "v1 (best)") || !strings.Contains(table, "| keep |") {
		t.Fatalf("table = %q, want v1 (best) and keep", table)
	}
}

func TestGateUnmeasuredGuardrailRefused(t *testing.T) {
	t.Parallel()
	fx := newFixture(t, "cost_usd")
	fx.baseline(rowsFor(0, allGrade(false), func(string) string { return `,"cost_usd":1` }))
	fx.changeP("two\n")
	fx.check("v1")
	fx.writeResults("v1", rowsFor(0, allGrade(true), noExtra)...)
	_, err := fx.gate("v1")
	const want = "guardrail cost_usd has fewer than 2 measured cases on train or test in v1; the gate cannot hold a guardrail it cannot measure"
	if err == nil || err.Error() != want {
		t.Fatalf("gate error = %v, want %q", err, want)
	}
}

func TestCheckExpectsEveryRep(t *testing.T) {
	t.Parallel()
	fx := newFixture(t)
	fx.s.Reps = 2
	fx.approve()
	res, err := fx.f.check(fx.s, "baseline")
	if err != nil {
		t.Fatal(err)
	}
	if res.ExpectedRows != 16 {
		t.Fatalf("expected rows = %d, want 16 for 4 train and 4 test cases at 2 reps", res.ExpectedRows)
	}
}
