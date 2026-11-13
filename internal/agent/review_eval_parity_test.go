package agent

import (
	json "encoding/json/v2"
	"errors"
	"fmt"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/decide"
)

func TestJudgeCaseTurnUsesTurnQuestionsAndChangedLines(t *testing.T) {
	c := corpusCase{Unit: "turn", Task: "change value", Files: []corpusFile{{Path: "a.go", Pre: "before\nold\nafter\n", Post: "before\nnew\nafter\n"}}}
	client := &judgeStub{}
	result := judgeCase(t.Context(), client, c, func(s string) (string, error) { return s, nil })
	if result.unit.err != nil {
		t.Fatal(result.unit.err)
	}
	if len(client.states) != 1 || !slices.Equal(client.asked[0], []string{"incomplete", "sound"}) {
		t.Fatalf("turn requests/questions: %d / %v", len(client.states), client.asked)
	}
	assertTurnChangedLineEvidence(t, client.states[0])
}

func assertTurnChangedLineEvidence(t *testing.T, encoded string) {
	t.Helper()
	var state turnState
	if err := json.Unmarshal([]byte(encoded), &state); err != nil {
		t.Fatal(err)
	}
	if len(state.Files) != 1 {
		t.Fatalf("files: %v", state.Files)
	}
	diff := state.Files[0].Diff
	if !strings.Contains(diff, "-old\n+new\n") || strings.Contains(diff, " before\n") || strings.Contains(diff, " after\n") {
		t.Fatalf("turn diff includes wrong evidence: %q", diff)
	}
	if strings.Contains(encoded, "\"part\"") || strings.Contains(encoded, "\"tool\"") {
		t.Fatalf("turn uses edit envelope: %s", encoded)
	}
}

func TestJudgeCaseLargeTurnIsOneBoundedUncertainRequest(t *testing.T) {
	c := corpusCase{Unit: "turn", Task: "replace values", Files: []corpusFile{{Path: "a.go", Pre: strings.Repeat("old value\n", 12000), Post: strings.Repeat("new value\n", 12000)}}}
	client := &judgeStub{}
	result := judgeCase(t.Context(), client, c, func(s string) (string, error) { return s, nil })
	if result.unit.err != nil {
		t.Fatal(result.unit.err)
	}
	if len(client.states) != 1 {
		t.Fatalf("turn sent %d requests, want one", len(client.states))
	}
	size, err := decide.RequestSize([]byte(client.states[0]), turnQuestionSet())
	if err != nil {
		t.Fatal(err)
	}
	if size > reviewRequestLimit {
		t.Fatalf("turn request bytes %d exceed %d", size, reviewRequestLimit)
	}
	if result.unit.verdict != reviewUncertain {
		t.Fatalf("partial turn judgment: %+v", result.unit)
	}
	if !strings.Contains(client.states[0], "[truncated:") {
		t.Fatal("large turn did not identify omitted evidence")
	}
}

func TestJudgeCaseTurnMasksAndChecksFinalState(t *testing.T) {
	const secret = "private-token-value"
	c := corpusCase{Unit: "turn", Task: secret, Files: []corpusFile{{Path: secret + ".go", Pre: "old\n", Post: secret + "\n"}}}
	client := &judgeStub{}
	mask := func(s string) (string, error) { return strings.ReplaceAll(s, secret, "{{secret:TOKEN}}"), nil }
	if result := judgeCase(t.Context(), client, c, mask); result.unit.err != nil {
		t.Fatal(result.unit.err)
	}
	if len(client.states) != 1 || strings.Contains(client.states[0], secret) {
		t.Fatalf("masked turn states: %v", client.states)
	}
	client = &judgeStub{}
	guard := func(s string) (string, error) {
		if strings.HasPrefix(s, "{") {
			return s + " ", nil
		}
		return s, nil
	}
	result := judgeCase(t.Context(), client, c, guard)
	if !errors.Is(result.unit.err, errReviewSecret) || len(client.states) != 0 {
		t.Fatalf("final mask guard: error %v, requests %d", result.unit.err, len(client.states))
	}
}

func TestHeldOutPreflightRejectsIneligibleTurnsBeforeAnyJudgeCall(t *testing.T) {
	identity := func(s string) (string, error) { return s, nil }
	for _, post := range []string{"", "old\n"} {
		t.Run("post="+post, func(t *testing.T) {
			valid := corpusCase{ID: "valid", Split: corpusSplitHeld, Unit: "turn", Task: "change", Files: []corpusFile{{Path: "a.go", Pre: "old\n", Post: "new\n"}}}
			invalid := corpusCase{ID: "invalid", Split: corpusSplitHeld, Unit: "turn", Task: "change", Files: []corpusFile{{Path: "b.go", Pre: "old\n", Post: post}}}
			client := &judgeStub{}
			totals, err := measureHeldOut(t.Context(), client, identity, []corpusCase{valid, invalid})
			if err == nil || !errors.Is(err, errTurnIneligible) || !strings.Contains(err.Error(), "case invalid: review measurement: ineligible turn input: no added lines") {
				t.Fatalf("preflight error: %v", err)
			}
			if totals != nil || len(client.states) != 0 {
				t.Fatalf("ineligible corpus produced totals %v and %d judge requests", totals, len(client.states))
			}
		})
	}
}

func TestHeldOutPreflightRejectsTurnCollapsedByMasking(t *testing.T) {
	c := corpusCase{ID: "collapsed", Split: corpusSplitHeld, Unit: "turn", Task: "change", Files: []corpusFile{{Path: "a.go", Pre: "old-token\n", Post: "new-token\n"}}}
	mask := func(s string) (string, error) {
		return strings.NewReplacer("old-token", "{{secret:TOKEN}}", "new-token", "{{secret:TOKEN}}").Replace(s), nil
	}
	client := &judgeStub{}
	totals, err := measureHeldOut(t.Context(), client, mask, []corpusCase{c})
	if !errors.Is(err, errTurnIneligible) || !strings.Contains(err.Error(), "ineligible turn input: no added lines") || totals != nil || len(client.states) != 0 {
		t.Fatalf("collapsed input: error %v, totals %v, requests %d", err, totals, len(client.states))
	}
}

func TestHeldOutPreflightRecordsTurnSizeOverflowAsErroredUnit(t *testing.T) {
	identity := func(s string) (string, error) { return s, nil }
	valid := corpusCase{ID: "valid", Split: corpusSplitHeld, Unit: "turn", Task: "change", Files: []corpusFile{{Path: "a.go", Pre: "old\n", Post: "new\n"}}}
	overflow := corpusCase{ID: "overflow", Split: corpusSplitHeld, Unit: "turn", Task: "change", Files: longPathTurnFiles(50, 700)}
	assertTurnMetadataOverflows(t, overflow, identity)
	for _, order := range []struct {
		name  string
		cases []corpusCase
	}{
		{name: "overflow first", cases: []corpusCase{overflow, valid}},
		{name: "overflow last", cases: []corpusCase{valid, overflow}},
	} {
		t.Run(order.name, func(t *testing.T) {
			client := &judgeStub{}
			totals, err := measureHeldOut(t.Context(), client, identity, order.cases)
			if err != nil || totals == nil {
				t.Fatalf("size overflow aborted the measurement: error %v, totals %v", err, totals)
			}
			assertErroredUnitAndJudgedNeighbour(t, totals, client, "overflow", strings.Repeat("p", 700))
		})
	}
}

func TestHeldOutPreflightIneligibleTurnStopsAfterErroredUnit(t *testing.T) {
	identity := func(s string) (string, error) { return s, nil }
	overflow := corpusCase{ID: "overflow", Split: corpusSplitHeld, Unit: "turn", Task: "change", Files: longPathTurnFiles(50, 700)}
	valid := corpusCase{ID: "valid", Split: corpusSplitHeld, Unit: "turn", Task: "change", Files: []corpusFile{{Path: "a.go", Pre: "old\n", Post: "new\n"}}}
	invalid := corpusCase{ID: "invalid", Split: corpusSplitHeld, Unit: "turn", Task: "change", Files: []corpusFile{{Path: "b.go", Pre: "old\n", Post: ""}}}
	client := &judgeStub{}
	totals, err := measureHeldOut(t.Context(), client, identity, []corpusCase{overflow, valid, invalid})
	if !errors.Is(err, errTurnIneligible) || totals != nil || len(client.states) != 0 {
		t.Fatalf("ineligible turn after an errored unit: error %v, totals %v, requests %d", err, totals, len(client.states))
	}
}

func TestHeldOutPreflightRecordsMaskFailureAsErroredUnit(t *testing.T) {
	mask := func(s string) (string, error) {
		if s == "bad.go" {
			return "", errors.New("masker down")
		}
		return s, nil
	}
	valid := corpusCase{ID: "valid", Split: corpusSplitHeld, Unit: "edit", Task: "change", Files: []corpusFile{{Path: "a.go", Kind: "code", Pre: "x := 1\n", Post: "x := 2\n"}}}
	bad := corpusCase{ID: "bad", Split: corpusSplitHeld, Unit: "edit", Task: "change", Files: []corpusFile{{Path: "bad.go", Kind: "code", Pre: "x := 1\n", Post: "x := 2\n"}}}
	client := &judgeStub{}
	totals, err := measureHeldOut(t.Context(), client, mask, []corpusCase{bad, valid})
	if err != nil || totals == nil {
		t.Fatalf("mask failure aborted the measurement: error %v, totals %v", err, totals)
	}
	assertErroredUnitAndJudgedNeighbour(t, totals, client, "bad", "bad.go")
}

func longPathTurnFiles(count, width int) []corpusFile {
	files := make([]corpusFile, 0, count)
	for i := range count {
		files = append(files, corpusFile{Path: fmt.Sprintf("%s%02d.go", strings.Repeat("p", width), i), Pre: "old\n", Post: "new\n"})
	}
	return files
}

func assertTurnMetadataOverflows(t *testing.T, c corpusCase, mask func(string) (string, error)) {
	t.Helper()
	paths := 0
	for _, f := range c.Files {
		paths += len(f.Path)
	}
	if paths <= reviewRequestLimit {
		t.Fatalf("fixture paths hold %d bytes, want more than %d", paths, reviewRequestLimit)
	}
	files, err := maskedCorpusFiles(c, mask)
	if err != nil {
		t.Fatal(err)
	}
	diffs, err := diffFiles(files, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, _, err := turnRequest(c.Task, diffs); !errors.Is(err, errReviewTurnSize) {
		t.Fatalf("production turn request error %v, want errReviewTurnSize", err)
	}
}

func assertErroredUnitAndJudgedNeighbour(t *testing.T, totals *evalTotals, client *judgeStub, id, hidden string) {
	t.Helper()
	if totals.cases != 2 || totals.errors != 1 || !slices.Equal(totals.erroredIDs, []string{id}) {
		t.Fatalf("totals %+v, want 2 cases and one errored unit %s", totals, id)
	}
	if len(client.states) != 1 || len(totals.latencies) != 1 || totals.partErrors != 0 {
		t.Fatalf("judge requests %d, latencies %d, part errors %d; want one judged neighbour", len(client.states), len(totals.latencies), totals.partErrors)
	}
	if !strings.Contains(client.states[0], `"a.go"`) || strings.Contains(client.states[0], hidden) {
		t.Fatalf("judged request is not the valid neighbour alone: %s", client.states[0])
	}
}

func TestHeldOutPreflightKeepsEligibleTurnEvidence(t *testing.T) {
	c := corpusCase{ID: "eligible", Split: corpusSplitHeld, Unit: "turn", Task: "change", Files: []corpusFile{
		{Path: "named.go", Pre: "unchanged\n", Post: "unchanged\n"},
		{Path: "a.go", Pre: "before\nold\nafter\n", Post: "before\nnew\nafter\n"},
	}}
	client := &judgeStub{}
	totals, err := measureHeldOut(t.Context(), client, func(s string) (string, error) { return s, nil }, []corpusCase{c})
	if err != nil || totals == nil || len(client.states) != 1 {
		t.Fatalf("eligible input: error %v, totals %v, requests %d", err, totals, len(client.states))
	}
	assertTurnChangedLineEvidence(t, client.states[0])
	assertEligibleTurnDiff(t, client.states[0])
	assertEligibleTurnRequest(t, client)
	if totals.cases != 1 || totals.errors != 0 || totals.partErrors != 0 || len(totals.latencies) != 1 {
		t.Fatalf("eligible totals: %+v", totals)
	}
}

func assertEligibleTurnDiff(t *testing.T, encoded string) {
	t.Helper()
	var state turnState
	if err := json.Unmarshal([]byte(encoded), &state); err != nil {
		t.Fatal(err)
	}
	if state.Files[0].Path != "a.go" || state.Files[0].Diff != "--- a/a.go\n+++ b/a.go\n@@ -2 +2 @@\n-old\n+new\n" || strings.Contains(encoded, "named.go") {
		t.Fatalf("changed-lines request: %s", encoded)
	}
}

func assertEligibleTurnRequest(t *testing.T, client *judgeStub) {
	t.Helper()
	if !slices.Equal(client.asked[0], []string{"incomplete", "sound"}) {
		t.Fatalf("turn questions: %v", client.asked)
	}
	size, err := decide.RequestSize([]byte(client.states[0]), turnQuestionSet())
	if err != nil || size > reviewRequestLimit {
		t.Fatalf("turn request size %d, limit %d, error %v", size, reviewRequestLimit, err)
	}
}
