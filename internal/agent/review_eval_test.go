package agent

import (
	"context"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"maps"
	"math"
	"os"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/decide"
)

const corpusErrorRate = 0.05

const (
	reviewLatencyMax  = 4 * time.Second
	gateRecall        = 0.5
	gateSoundFlagRate = 0.02
)

var errTurnIneligible = errors.New("review measurement: ineligible turn input: no added lines")

type questionTally struct {
	tp, fp, fn int
}

type evalTotals struct {
	cases, errors, partErrors     int
	soundParts, soundPartsFlagged int
	latencies                     []time.Duration
	tally                         map[string]*questionTally
	erroredIDs                    []string
}

type caseResult struct {
	parts     []judgment
	unit      judgment
	latencies []time.Duration
}

type preparedCase struct {
	parts []judgeInput
	err   error
}

func askJudgePart(ctx context.Context, client decide.Client, part judgeInput) judgment {
	answers, err := client.Ask(ctx, part.state, part.questions)
	if err != nil {
		return judgment{err: err}
	}
	scores, err := scoreAnswers(answers, part.questions)
	if err != nil {
		return judgment{err: err}
	}
	verdict, flags := reviewVerdict(scores)
	return judgment{scores: scores, verdict: verdict, flags: flags, spoken: reviewSpoken(scores, flags, nil)}
}

func prepareCase(c corpusCase, mask func(string) (string, error)) ([]judgeInput, error) {
	files, err := maskedCorpusFiles(c, mask)
	if err != nil {
		return nil, err
	}
	task, err := mask(c.Task)
	if err != nil {
		return nil, errReviewMask
	}
	parts, err := maskedParts(mask, task, files, c.Unit)
	if err != nil {
		return nil, err
	}
	if len(parts) == 0 {
		return nil, errReviewNoPart
	}
	return parts, nil
}

func judgeCase(ctx context.Context, client decide.Client, c corpusCase, mask func(string) (string, error)) caseResult {
	parts, err := prepareCase(c, mask)
	return preparedCase{parts: parts, err: err}.judge(ctx, client, c)
}

func (p preparedCase) judge(ctx context.Context, client decide.Client, c corpusCase) caseResult {
	if p.err != nil {
		return caseResult{unit: judgment{err: p.err}}
	}
	return judgePreparedCase(ctx, client, c, p.parts)
}

func judgePreparedCase(ctx context.Context, client decide.Client, c corpusCase, parts []judgeInput) caseResult {
	res := caseResult{parts: make([]judgment, 0, len(parts))}
	for _, part := range parts {
		start := time.Now()
		j := askJudgePart(ctx, client, part)
		if c.Unit == "turn" && part.truncated {
			j = truncatedTurnJudgment(j)
		}
		res.parts = append(res.parts, j)
		res.latencies = append(res.latencies, time.Since(start))
	}
	res.unit = combineParts(res.parts)
	return res
}

func truncatedTurnJudgment(j judgment) judgment {
	j.spoken = nil
	if j.err == nil && j.verdict == reviewClean {
		j.verdict = reviewUncertain
	}
	return j
}

func maskedCorpusFiles(c corpusCase, mask func(string) (string, error)) ([]reviewFile, error) {
	out := make([]reviewFile, 0, len(c.Files))
	for _, f := range c.Files {
		masked, err := maskedFile(f, mask)
		if err != nil {
			return nil, err
		}
		out = append(out, classifyReviewFile(masked.Path, corpusChange(f), []byte(masked.Pre), []byte(masked.Post)))
	}
	return out, nil
}

func maskedFile(f corpusFile, mask func(string) (string, error)) (corpusFile, error) {
	path, err := mask(f.Path)
	if err != nil {
		return corpusFile{}, errReviewMask
	}
	pre, err := mask(f.Pre)
	if err != nil {
		return corpusFile{}, errReviewMask
	}
	post, err := mask(f.Post)
	if err != nil {
		return corpusFile{}, errReviewMask
	}
	return corpusFile{Path: path, Kind: f.Kind, Pre: pre, Post: post}, nil
}

func maskedParts(mask func(string) (string, error), task string, files []reviewFile, unit string) ([]judgeInput, error) {
	contextLines := reviewContextLines
	if unit == "turn" {
		contextLines = 0
	}
	diffs, err := diffFiles(files, contextLines)
	if err != nil {
		return nil, err
	}
	if unit == "turn" {
		part, added, requestErr := turnRequest(task, diffs)
		if requestErr != nil {
			return nil, requestErr
		}
		if added == 0 {
			return nil, errTurnIneligible
		}
		if maskErr := requireMasked(mask, part.state); maskErr != nil {
			return nil, maskErr
		}
		for _, file := range files {
			part.truncated = part.truncated || file.skip == "large"
		}
		return []judgeInput{part}, nil
	}
	parts, _, err := packMasked(mask, task, corpusTool, diffs)
	return parts, err
}

func (e *evalTotals) addParts(c corpusCase, parts []judgment) {
	for _, p := range parts {
		switch {
		case p.err != nil:
			e.partErrors++
		case len(c.Flags) == 0:
			e.soundParts++
			if len(p.flags) > 0 {
				e.soundPartsFlagged++
			}
		}
	}
}

func (e *evalTotals) addTally(c corpusCase, flags []string) {
	for _, q := range reviewQuestions {
		if q.id == soundID {
			continue
		}
		t := e.tally[q.id]
		positive, flagged := slices.Contains(c.Flags, q.id), slices.Contains(flags, q.id)
		switch {
		case positive && flagged:
			t.tp++
		case positive:
			t.fn++
		case flagged:
			t.fp++
		}
	}
}

func (e *evalTotals) add(c corpusCase, r caseResult) {
	e.cases++
	e.latencies = append(e.latencies, r.latencies...)
	e.addParts(c, r.parts)
	if r.unit.err != nil {
		e.errors++
		e.erroredIDs = append(e.erroredIDs, c.ID)
		return
	}
	e.addTally(c, r.unit.flags)
}

func (t *questionTally) row() ratchetQuestion {
	row := ratchetQuestion{TrueFlags: t.tp}
	if t.tp+t.fp > 0 {
		p := float64(t.tp) / float64(t.tp+t.fp)
		row.Precision = &p
	}
	if t.tp+t.fn > 0 {
		r := float64(t.tp) / float64(t.tp+t.fn)
		row.Recall = &r
	}
	return row
}

func (e *evalTotals) measured() ratchetMeasured {
	m := ratchetMeasured{Errors: e.errors, Questions: map[string]ratchetQuestion{}}
	if e.soundParts > 0 {
		m.SoundFlagRate = float64(e.soundPartsFlagged) / float64(e.soundParts)
	}
	for id, t := range e.tally {
		m.Questions[id] = t.row()
	}
	return m
}

func p95(d []time.Duration) time.Duration {
	if len(d) == 0 {
		return 0
	}
	sorted := slices.Clone(d)
	slices.Sort(sorted)
	return sorted[int(math.Ceil(0.95*float64(len(sorted))))-1]
}

func checkRatchet(t *testing.T, recorded *ratchetMeasured, got ratchetMeasured) bool {
	t.Helper()
	problems := ratchetProblems(recorded, got)
	for _, p := range problems {
		t.Error(p)
	}
	return len(problems) == 0
}

func ratchetProblems(recorded *ratchetMeasured, got ratchetMeasured) []string {
	if recorded == nil {
		return nil
	}
	var out []string
	if got.SoundFlagRate > recorded.SoundFlagRate {
		out = append(out, fmt.Sprintf("sound flag rate %.3f exceeds recorded %.3f", got.SoundFlagRate, recorded.SoundFlagRate))
	}
	for _, id := range slices.Sorted(maps.Keys(recorded.Questions)) {
		prev := recorded.Questions[id]
		row, ok := got.Questions[id]
		switch {
		case !ok:
			out = append(out, fmt.Sprintf("question %s is missing from the run", id))
		case prev.Precision == nil:
		case row.Precision == nil:
			out = append(out, fmt.Sprintf("precision of %s is absent; recorded %.3f", id, *prev.Precision))
		case *row.Precision < *prev.Precision:
			out = append(out, fmt.Sprintf("precision of %s fell from %.3f to %.3f", id, *prev.Precision, *row.Precision))
		}
	}
	return out
}

func gateFailures(m ratchetMeasured) []string {
	var out []string
	if m.SoundFlagRate > gateSoundFlagRate {
		out = append(out, fmt.Sprintf("sound flag rate %.3f above %.3f", m.SoundFlagRate, gateSoundFlagRate))
	}
	for _, q := range reviewQuestions {
		if q.id == soundID {
			continue
		}
		row, ok := m.Questions[q.id]
		if !ok {
			out = append(out, q.id+": no row")
			continue
		}
		out = append(out, precisionFailure(q.id, row.Precision)...)
		out = append(out, recallFailure(q.id, row.Recall)...)
	}
	return out
}

func precisionFailure(id string, p *float64) []string {
	switch {
	case p == nil:
		return []string{id + ": precision absent"}
	case *p < corpusThreshold:
		return []string{fmt.Sprintf("%s: precision %.3f below %.3f", id, *p, corpusThreshold)}
	}
	return nil
}

func recallFailure(id string, r *float64) []string {
	switch {
	case r == nil:
		return []string{id + ": recall absent"}
	case *r < gateRecall:
		return []string{fmt.Sprintf("%s: recall %.3f below %.3f", id, *r, gateRecall)}
	}
	return nil
}

func tallyLines(e *evalTotals) []string {
	var out []string
	for _, q := range reviewQuestions {
		if q.id == soundID {
			continue
		}
		row := e.tally[q.id].row()
		out = append(out, fmt.Sprintf("%s: tp %d fp %d fn %d precision %s recall %s", q.id, e.tally[q.id].tp, e.tally[q.id].fp, e.tally[q.id].fn, optionalText(row.Precision), optionalText(row.Recall)))
	}
	if len(e.erroredIDs) == 0 {
		return append(out, "errored cases: none")
	}
	return append(out, "errored cases: "+strings.Join(e.erroredIDs, ", "))
}

func optionalText(v *float64) string {
	if v == nil {
		return "absent"
	}
	return fmt.Sprintf("%.3f", *v)
}

func heldOutClient(model, key string) decide.Client {
	return decide.NewDecisions(decide.DecisionsConfig{
		Model: model,
		Token: func(context.Context) (string, error) { return key, nil },
	})
}

func prepareHeldOut(mask func(string) (string, error), cases []corpusCase) ([]preparedCase, error) {
	prepared := make([]preparedCase, len(cases))
	for i, c := range cases {
		if c.Split != corpusSplitHeld {
			continue
		}
		parts, err := prepareCase(c, mask)
		if errors.Is(err, errTurnIneligible) {
			return nil, fmt.Errorf("case %s: %w", c.ID, err)
		}
		prepared[i] = preparedCase{parts: parts, err: err}
	}
	return prepared, nil
}

func measureHeldOut(ctx context.Context, client decide.Client, mask func(string) (string, error), cases []corpusCase) (*evalTotals, error) {
	prepared, err := prepareHeldOut(mask, cases)
	if err != nil {
		return nil, err
	}
	totals := &evalTotals{tally: map[string]*questionTally{}}
	for _, q := range reviewQuestions {
		totals.tally[q.id] = &questionTally{}
	}
	for i, c := range cases {
		if c.Split == corpusSplitHeld {
			totals.add(c, prepared[i].judge(ctx, client, c))
		}
	}
	return totals, nil
}

func reportLatency(t *testing.T, totals *evalTotals) {
	t.Helper()
	latency := p95(totals.latencies)
	t.Logf("per-part latency p95 %s over %d parts; part errors %d", latency, len(totals.latencies), totals.partErrors)
	if latency > reviewLatencyMax {
		t.Logf("per-part p95 exceeds %s: set reviewMaxParts to 4 before the feature is enabled by default", reviewLatencyMax)
	}
}

func reportTallies(t *testing.T, totals *evalTotals) {
	t.Helper()
	for _, line := range tallyLines(totals) {
		t.Log(line)
	}
}

func heldOutCredentials() (string, string, bool) {
	model, ok := strings.CutPrefix(os.Getenv("DALGONA_EDIT_REVIEW_EVAL"), "decisions:")
	key := os.Getenv("OPENROUTER_API_KEY")
	return model, key, ok && model != "" && key != ""
}

func checkErrorRate(t *testing.T, totals *evalTotals) {
	t.Helper()
	if totals.cases > 0 && float64(totals.errors)/float64(totals.cases) > corpusErrorRate {
		t.Fatalf("%d of %d held-out cases errored; the limit is 5 percent", totals.errors, totals.cases)
	}
}

func TestEditReviewHeldOutPrecision(t *testing.T) {
	model, key, ok := heldOutCredentials()
	if !ok {
		t.Skip("set DALGONA_EDIT_REVIEW_EVAL=decisions:<id> and OPENROUTER_API_KEY to measure held-out precision")
	}
	ratchet := readRatchet(t)
	store, err := auth.NewWithPatterns(t.TempDir(), nil)
	if err != nil {
		t.Fatalf("masker: %v", err)
	}
	totals, err := measureHeldOut(t.Context(), heldOutClient(model, key), store.MaskWithError, readCorpus(t))
	if err != nil {
		t.Fatal(err)
	}
	got := totals.measured()
	reportLatency(t, totals)
	reportTallies(t, totals)
	for _, failure := range gateFailures(got) {
		t.Logf("default-on gate: %s", failure)
	}
	checkErrorRate(t, totals)
	if !checkRatchet(t, ratchet.Measured, got) || os.Getenv("DALGONA_EDIT_REVIEW_RATCHET") != "update" {
		return
	}
	ratchet.Measured = &got
	out, err := marshalRatchet(ratchet)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(corpusRatchetPath, out, 0o644); err != nil {
		t.Fatalf("write ratchet: %v", err)
	}
}

func passingMeasured() ratchetMeasured {
	one := 1.0
	m := ratchetMeasured{Questions: map[string]ratchetQuestion{}}
	for _, q := range reviewQuestions {
		if q.id != soundID {
			m.Questions[q.id] = ratchetQuestion{Precision: &one, Recall: &one, TrueFlags: 5}
		}
	}
	return m
}

func TestHeldOutReportCountsFalseFlagsAndErrors(t *testing.T) {
	totals := &evalTotals{tally: map[string]*questionTally{}}
	for _, q := range reviewQuestions {
		totals.tally[q.id] = &questionTally{}
	}
	totals.add(corpusCase{ID: "a", Flags: []string{"lost_code"}}, caseResult{unit: judgment{flags: []string{"lost_code"}}})
	totals.add(corpusCase{ID: "b"}, caseResult{unit: judgment{flags: []string{"wrong_logic"}}})
	totals.add(corpusCase{ID: "c", Flags: []string{"placeholder"}}, caseResult{unit: judgment{}})
	totals.add(corpusCase{ID: "d", Flags: []string{"unsafe_input"}}, caseResult{unit: judgment{err: errReviewNoPart}})
	want := []string{
		"lost_code: tp 1 fp 0 fn 0 precision 1.000 recall 1.000",
		"wrong_logic: tp 0 fp 1 fn 0 precision 0.000 recall absent",
		"placeholder: tp 0 fp 0 fn 1 precision absent recall 0.000",
		"unsafe_input: tp 0 fp 0 fn 0 precision absent recall absent",
		"errored cases: d",
	}
	got := tallyLines(totals)
	for _, line := range want {
		if !slices.Contains(got, line) {
			t.Errorf("report lacks %q; report is %v", line, got)
		}
	}
}

func TestRecallIsAbsentWithoutPositives(t *testing.T) {
	tally := &questionTally{fp: 1}
	if row := tally.row(); row.Recall != nil {
		t.Fatalf("recall of a question with no positive case is %v, want absent", *row.Recall)
	}
	out, err := json.Marshal(ratchetQuestion{TrueFlags: 0})
	if err != nil {
		t.Fatal(err)
	}
	if want := `{"precision":null,"recall":null,"true_flags":0}`; string(out) != want {
		t.Fatalf("absent row encodes as %s, want %s", out, want)
	}
}

func TestDefaultOnGateTreatsAbsentAsNotPassing(t *testing.T) {
	if failures := gateFailures(passingMeasured()); len(failures) != 0 {
		t.Fatalf("a passing run fails the gate: %v", failures)
	}
	cases := []struct {
		name   string
		mutate func(m *ratchetMeasured)
		want   string
	}{
		{name: "recall absent", mutate: func(m *ratchetMeasured) {
			row := m.Questions["lost_code"]
			row.Recall = nil
			m.Questions["lost_code"] = row
		}, want: "lost_code: recall absent"},
		{name: "precision absent", mutate: func(m *ratchetMeasured) {
			row := m.Questions["wrong_logic"]
			row.Precision = nil
			m.Questions["wrong_logic"] = row
		}, want: "wrong_logic: precision absent"},
		{name: "question missing", mutate: func(m *ratchetMeasured) { delete(m.Questions, "placeholder") }, want: "placeholder: no row"},
		{name: "sound flag rate", mutate: func(m *ratchetMeasured) { m.SoundFlagRate = 0.03 }, want: "sound flag rate 0.030 above 0.020"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			m := passingMeasured()
			c.mutate(&m)
			if failures := gateFailures(m); !slices.Contains(failures, c.want) {
				t.Fatalf("gate reports %v, want %q", failures, c.want)
			}
		})
	}
}

func TestRatchetProblemsCoverEveryRecordedQuestion(t *testing.T) {
	high, low := 0.9, 0.8
	recorded := &ratchetMeasured{Questions: map[string]ratchetQuestion{
		"lost_code":   {Precision: &high, TrueFlags: 5},
		"wrong_logic": {Precision: &high, TrueFlags: 5},
	}}
	cases := []struct {
		name string
		got  ratchetMeasured
		want []string
	}{
		{
			name: "missing question", got: ratchetMeasured{Questions: map[string]ratchetQuestion{"lost_code": {Precision: &high}}},
			want: []string{"question wrong_logic is missing from the run"},
		},
		{
			name: "precision fell", got: ratchetMeasured{Questions: map[string]ratchetQuestion{"lost_code": {Precision: &high}, "wrong_logic": {Precision: &low}}},
			want: []string{"precision of wrong_logic fell from 0.900 to 0.800"},
		},
		{
			name: "precision absent", got: ratchetMeasured{Questions: map[string]ratchetQuestion{"lost_code": {Precision: nil}, "wrong_logic": {Precision: &high}}},
			want: []string{"precision of lost_code is absent; recorded 0.900"},
		},
		{name: "held", got: ratchetMeasured{Questions: map[string]ratchetQuestion{"lost_code": {Precision: &high}, "wrong_logic": {Precision: &high}}}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := ratchetProblems(recorded, c.got); !slices.Equal(got, c.want) {
				t.Fatalf("ratchetProblems = %v, want %v", got, c.want)
			}
		})
	}
}

func TestJudgeCaseMasksTheStateLikeTheLivePath(t *testing.T) {
	const secret = "token-abcdefgh"
	c := corpusCase{ID: "m", Group: groupSoundEdit, Unit: "edit", Task: "fix it", Files: []corpusFile{
		{Path: "a.go", Kind: "code", Pre: "x := 1\n", Post: "x := " + secret + "\n"},
	}}
	mask := func(text string) (string, error) {
		return strings.ReplaceAll(text, secret, "{{secret:TOKEN}}"), nil
	}
	client := &judgeStub{}
	if res := judgeCase(t.Context(), client, c, mask); res.unit.err != nil {
		t.Fatalf("masked case errored: %v", res.unit.err)
	}
	if len(client.states) == 0 {
		t.Fatal("the judge was never asked")
	}
	for _, state := range client.states {
		if strings.Contains(state, secret) {
			t.Fatalf("judge state holds the raw secret: %s", state)
		}
	}
	broken := func(string) (string, error) { return "", errors.New("masker down") }
	if res := judgeCase(t.Context(), &judgeStub{}, c, broken); !errors.Is(res.unit.err, errReviewMask) {
		t.Fatalf("a failing masker gives %v, want errReviewMask", res.unit.err)
	}
}
