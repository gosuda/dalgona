package ratchet

import (
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"maps"
	"math"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"time"
)

const variantPattern = `^(baseline|v[1-9][0-9]{0,3})$`

var variantName = regexp.MustCompile(variantPattern)

type checkResult struct {
	Variant      string   `json:"variant"`
	Command      string   `json:"command"`
	ExpectedRows int      `json:"expected_rows"`
	OutDir       string   `json:"out_dir"`
	Reps         int      `json:"reps"`
	ChangedFiles []string `json:"changed_files"`
}

type gateResult struct {
	Variant      string             `json:"variant"`
	Decision     string             `json:"decision"`
	Reasons      []string           `json:"reasons"`
	Warnings     []string           `json:"warnings"`
	Deltas       map[string]pair    `json:"deltas"`
	Train        map[string]summary `json:"train"`
	Test         map[string]summary `json:"test"`
	Best         *bestRound         `json:"best"`
	Plateau      bool               `json:"plateau"`
	Done         bool               `json:"done"`
	DoneReason   string             `json:"done_reason"`
	FlatRounds   int                `json:"flat_rounds"`
	ChangedFiles []string           `json:"changed_files"`
	Table        string             `json:"table"`
}

type trainView struct {
	Variant string                         `json:"variant"`
	Rows    []jsontext.Value               `json:"rows"`
	Traces  []string                       `json:"traces"`
	History map[string]map[string]*float64 `json:"history"`
}

type outcome struct {
	decision string
	reasons  []string
	warnings []string
	deltas   map[string]pair
}

type scoring struct {
	metrics []string
	means   map[string]caseMeans
	train   map[string]summary
	test    map[string]summary
}

type gateInput struct {
	known       map[string]bool
	rows        []resultRow
	erroredOnly []string
	sc          scoring
	changeSum   string
	changed     []fileSHA
}

type recording struct {
	variant string
	change  string
	data    variantData
	in      gateInput
	out     outcome
}

func (f *flow) check(s *state, variant string) (checkResult, error) {
	if !variantName.MatchString(variant) {
		return checkResult{}, fmt.Errorf("variant must match %s: %s", variantPattern, variant)
	}
	if done, reason := s.done(); done {
		return checkResult{}, fmt.Errorf("flow is done: %s; change stop with plan and approve the plan again, or start a new flow", reason)
	}
	if err := f.requireApprovals(s); err != nil {
		return checkResult{}, err
	}
	if err := f.requireBaselineUnchanged(s); err != nil {
		return checkResult{}, err
	}
	if want := expectedVariant(s); variant != want {
		return checkResult{}, fmt.Errorf("next variant is %s, got %s", want, variant)
	}
	changeSum, changed, err := f.hashPaths(s.ChangePaths)
	if err != nil {
		return checkResult{}, err
	}
	if err := s.checkChange(variant, changeSum); err != nil {
		return checkResult{}, err
	}
	s.Pending = &pending{Variant: variant, ChangeSHA: changeSum, At: time.Now().UTC()}
	if err := f.save(s); err != nil {
		return checkResult{}, err
	}
	return checkResult{
		Variant:      variant,
		Command:      f.command(s, variant),
		ExpectedRows: (len(s.TrainIDs) + len(s.TestIDs)) * s.Reps,
		OutDir:       filepath.ToSlash(filepath.Join(f.dir, variant)),
		Reps:         s.Reps,
		ChangedFiles: filePaths(changed),
	}, nil
}

func (f *flow) command(s *state, variant string) string {
	return strings.NewReplacer("{variant}", variant, "{flow_dir}", filepath.ToSlash(f.dir)).Replace(s.Command)
}

func expectedVariant(s *state) string {
	last, ok := s.lastRound()
	switch {
	case !ok:
		return "baseline"
	case last.Decision == "rerun":
		return last.Variant
	}
	return fmt.Sprintf("v%d", last.Round+1)
}

func (s *state) checkChange(variant, sum string) error {
	if variant == "baseline" {
		return nil
	}
	last, _ := s.lastRound()
	if last.Decision == "rerun" && last.Variant == variant {
		if sum != last.ChangeSHA {
			return fmt.Errorf("%s awaits rerun; restore the change paths to the state gated in the last round", variant)
		}
		return nil
	}
	best, err := s.bestRecord()
	if err != nil {
		return err
	}
	if sum == best.ChangeSHA {
		return fmt.Errorf("%s changes nothing in the change paths since the best round (%s)", variant, s.Best.Variant)
	}
	return nil
}

func (s *state) bestRecord() (round, error) {
	best, ok := s.round(s.Best.Round)
	if !ok {
		return round{}, fmt.Errorf("best round %d is missing", s.Best.Round)
	}
	return best, nil
}

func (f *flow) requireApprovals(s *state) error {
	status, err := f.approvals(s)
	if err != nil {
		return err
	}
	var stale []string
	for _, stage := range stages {
		if status[stage] != "current" {
			stale = append(stale, stage+": "+status[stage])
		}
	}
	if len(stale) > 0 {
		return fmt.Errorf("approvals are not current (%s); call approve for each stage", strings.Join(stale, ", "))
	}
	return nil
}

func (f *flow) requireBaselineUnchanged(s *state) error {
	if s.BaselineInputsSHA != "" {
		sum, err := f.digest(s, stageInputs)
		if err != nil {
			return err
		}
		if sum != s.BaselineInputsSHA {
			return errors.New("the cases changed after the baseline was gated; start a new flow with init under a new name")
		}
	}
	if s.OffLimitsSHA != "" {
		sum, _, err := f.hashPaths(s.OffLimits)
		if err != nil {
			return err
		}
		if sum != s.OffLimitsSHA {
			return errors.New("off-limits files changed since the baseline; restore them before the next round")
		}
	}
	return nil
}

func (f *flow) gate(s *state, variant, change string) (gateResult, error) {
	data, err := f.readVariant(variant)
	if err != nil {
		return gateResult{}, err
	}
	if last, ok := s.lastRound(); ok && last.Variant == variant && last.ResultsSHA == data.ResultsSHA {
		return s.result(last), nil
	}
	in, err := f.gateInputs(s, variant, data)
	if err != nil {
		return gateResult{}, err
	}
	out, err := f.judge(s, variant, in)
	if err != nil {
		return gateResult{}, err
	}
	r, err := f.record(s, recording{variant: variant, change: change, data: data, in: in, out: out})
	if err != nil {
		return gateResult{}, err
	}
	if err = f.save(s); err != nil {
		return gateResult{}, err
	}
	return s.result(r), nil
}

func (f *flow) gateInputs(s *state, variant string, data variantData) (gateInput, error) {
	changeSum, changed, err := f.checkPending(s, variant)
	if err != nil {
		return gateInput{}, err
	}
	if err = f.requireApprovals(s); err != nil {
		return gateInput{}, err
	}
	erroredOnly, err := checkSlots(s, variant, data)
	if err != nil {
		return gateInput{}, err
	}
	if err = checkLeaks(s, variant, data); err != nil {
		return gateInput{}, err
	}
	known := idSet(slices.Concat(s.TrainIDs, s.TestIDs))
	rows := filterRows(data.Rows, known)
	sc := s.score(rows)
	if err = requireTarget(s, sc); err != nil {
		return gateInput{}, err
	}
	if err = requireMeasured(s, sc, variant); err != nil {
		return gateInput{}, err
	}
	return gateInput{known: known, rows: rows, erroredOnly: erroredOnly, sc: sc, changeSum: changeSum, changed: changed}, nil
}

func (f *flow) checkPending(s *state, variant string) (string, []fileSHA, error) {
	if s.Pending == nil || s.Pending.Variant != variant {
		return "", nil, fmt.Errorf("call check(%s) before gate(%s)", variant, variant)
	}
	changeSum, changed, err := f.hashPaths(s.ChangePaths)
	if err != nil {
		return "", nil, err
	}
	if changeSum != s.Pending.ChangeSHA {
		return "", nil, fmt.Errorf("the change paths changed after check(%s); call check again and run %s again", variant, variant)
	}
	return changeSum, changed, nil
}

func (s *state) rerunning(variant string) bool {
	last, ok := s.lastRound()
	return ok && last.Variant == variant && last.Decision == "rerun"
}

func (s *state) rerunsOf(variant string) int {
	last, ok := s.lastRound()
	if ok && last.Variant == variant {
		return last.Reruns
	}
	return 0
}

func (s *state) roundNumber(variant string) int {
	last, ok := s.lastRound()
	switch {
	case !ok:
		return 0
	case s.rerunning(variant):
		return last.Round
	}
	return last.Round + 1
}

func (s *state) score(rows []resultRow) scoring {
	metrics := slices.Concat([]string{s.Goal.Target}, s.Goal.Hold)
	sc := scoring{
		metrics: metrics,
		means:   make(map[string]caseMeans, len(metrics)),
		train:   make(map[string]summary, len(metrics)),
		test:    make(map[string]summary, len(metrics)),
	}
	for _, m := range metrics {
		means := meansByCase(rows, m)
		sc.means[m] = means
		sc.train[m] = summarize(means, s.TrainIDs)
		sc.test[m] = summarize(means, s.TestIDs)
	}
	return sc
}

func requireTarget(s *state, sc scoring) error {
	target := s.Goal.Target
	if sc.train[target].N == 0 {
		return fmt.Errorf("no train row carries the target metric %q", target)
	}
	if sc.test[target].N == 0 {
		return fmt.Errorf("no test row carries the target metric %q", target)
	}
	return nil
}

func requireMeasured(s *state, sc scoring, variant string) error {
	var unmeasured []string
	for _, m := range s.Goal.Hold {
		if sc.train[m].N < 2 || sc.test[m].N < 2 {
			unmeasured = append(unmeasured, m)
		}
	}
	if len(unmeasured) > 0 {
		return fmt.Errorf("guardrail %s has fewer than 2 measured cases on train or test in %s; the gate cannot hold a guardrail it cannot measure", strings.Join(unmeasured, ", "), variant)
	}
	return nil
}

func (f *flow) judge(s *state, variant string, in gateInput) (outcome, error) {
	switch {
	case variant == "baseline":
		return outcome{decision: "baseline", warnings: baselineWarnings(s, in.sc, in.rows)}, nil
	case len(in.erroredOnly) > 0:
		if s.rerunsOf(variant) >= maxReruns {
			return outcome{decision: "revert", reasons: []string{fmt.Sprintf("rerun limit reached: %s was rerun %d times", variant, maxReruns)}}, nil
		}
		reason := fmt.Sprintf("%d slots only errored (%s); resume %s until every slot is scored, then gate it again", len(in.erroredOnly), listed(in.erroredOnly), variant)
		return outcome{decision: "rerun", reasons: []string{reason}}, nil
	}
	return f.climb(s, variant, in)
}

func (f *flow) climb(s *state, variant string, in gateInput) (outcome, error) {
	reference := s.Best.Variant
	refData, err := f.readVariant(reference)
	if err != nil {
		return outcome{}, err
	}
	refRows := filterRows(refData.Rows, in.known)
	deltas := make(map[string]pair, len(in.sc.metrics))
	for _, m := range in.sc.metrics {
		dir := metricDirection(s.Goal, m)
		ref := meansByCase(refRows, m)
		deltas[m] = pair{
			Train: paired(in.sc.means[m], ref, s.TrainIDs, dir),
			Test:  paired(in.sc.means[m], ref, s.TestIDs, dir),
		}
	}
	if err := requirePaired(s.Goal.Hold, deltas, variant, reference); err != nil {
		return outcome{}, err
	}
	guards := make([]guard, 0, len(s.Goal.Hold))
	for _, m := range s.Goal.Hold {
		guards = append(guards, guard{Metric: m, pair: deltas[m]})
	}
	decision, reason := decideRound(s.Goal.Target, deltas[s.Goal.Target], guards, reference, variant, s.rerunsOf(variant))
	return outcome{decision: decision, reasons: []string{reason}, deltas: deltas}, nil
}

func requirePaired(hold []string, deltas map[string]pair, variant, reference string) error {
	var unpaired []string
	for _, m := range hold {
		if deltas[m].Train.N < 2 || deltas[m].Test.N < 2 {
			unpaired = append(unpaired, m)
		}
	}
	if len(unpaired) > 0 {
		return fmt.Errorf("guardrail %s has fewer than 2 cases measured in both %s and %s on train or test; the gate cannot compare it", strings.Join(unpaired, ", "), variant, reference)
	}
	return nil
}

func metricDirection(g goal, metric string) string {
	if metric == g.Target {
		return g.Direction
	}
	if direction, ok := g.Directions[metric]; ok {
		return direction
	}
	return defaultDirection(metric)
}

func baselineWarnings(s *state, sc scoring, rows []resultRow) []string {
	target := s.Goal.Target
	var warnings []string
	if nearCeiling(s, sc, rows) {
		warnings = append(warnings, fmt.Sprintf("target %s is near its ceiling in the baseline", target))
	}
	if diverges(sc.train[target], sc.test[target]) {
		warnings = append(warnings, fmt.Sprintf("baseline %s train and test differ by more than their intervals", target))
	}
	return warnings
}

func nearCeiling(s *state, sc scoring, rows []resultRow) bool {
	target := s.Goal.Target
	overall := summarize(sc.means[target], slices.Concat(s.TrainIDs, s.TestIDs))
	return s.Goal.Direction == directionH && inUnitInterval(rows, target) && overall.Mean != nil && *overall.Mean >= 0.95
}

func diverges(train, test summary) bool {
	return train.Mean != nil && test.Mean != nil && train.Half != nil && test.Half != nil &&
		math.Abs(*train.Mean-*test.Mean) > math.Hypot(*train.Half, *test.Half)
}

func inUnitInterval(rows []resultRow, metric string) bool {
	for _, row := range rows {
		if value, ok := row.Metrics[metric]; ok && (value < 0 || value > 1) {
			return false
		}
	}
	return true
}

func (f *flow) record(s *state, rec recording) (round, error) {
	r, err := s.buildRound(rec)
	if err != nil {
		return round{}, err
	}
	if r.Decision == "baseline" {
		if err = f.recordBaseline(s); err != nil {
			return round{}, err
		}
	}
	s.commitRound(r)
	return r, nil
}

func (s *state) buildRound(rec recording) (round, error) {
	in, out := rec.in, rec.out
	r := round{
		Round:       s.roundNumber(rec.variant),
		Variant:     rec.variant,
		Change:      rec.change,
		Decision:    out.decision,
		Reasons:     out.reasons,
		Warnings:    append(dataWarnings(rec.data, in.known), out.warnings...),
		Train:       in.sc.train,
		Test:        in.sc.test,
		Deltas:      out.deltas,
		Errors:      len(rec.data.Errors),
		Truncated:   countTruncated(in.rows),
		Reruns:      s.rerunsOf(rec.variant),
		ResultsSHA:  rec.data.ResultsSHA,
		ChangeSHA:   in.changeSum,
		ChangeFiles: in.changed,
		At:          time.Now().UTC(),
	}
	if rec.variant == "baseline" {
		r.Change = "baseline"
	}
	if r.Decision == "rerun" {
		r.Reruns++
	}
	if s.Best != nil {
		best, err := s.bestRecord()
		if err != nil {
			return round{}, err
		}
		r.ChangedFiles = changedFiles(best.ChangeFiles, in.changed)
	}
	return r, nil
}

func (s *state) commitRound(r round) {
	if s.rerunning(r.Variant) {
		s.Rounds[len(s.Rounds)-1] = r
	} else {
		s.Rounds = append(s.Rounds, r)
	}
	s.advance(r.Decision)
	if r.Decision == "baseline" || r.Decision == "keep" {
		s.Best = &bestRound{Round: r.Round, Variant: r.Variant, TestScore: r.Test[s.Goal.Target].Mean}
	}
	if r.Decision != "rerun" {
		s.Pending = nil
	}
}

func (f *flow) recordBaseline(s *state) error {
	inputs, err := f.digest(s, stageInputs)
	if err != nil {
		return err
	}
	offLimits, _, err := f.hashPaths(s.OffLimits)
	if err != nil {
		return err
	}
	s.BaselineInputsSHA = inputs
	s.OffLimitsSHA = offLimits
	return nil
}

func (s *state) result(r round) gateResult {
	done, reason := s.done()
	return gateResult{
		Variant:      r.Variant,
		Decision:     r.Decision,
		Reasons:      r.Reasons,
		Warnings:     r.Warnings,
		Deltas:       r.Deltas,
		Train:        r.Train,
		Test:         r.Test,
		Best:         s.Best,
		Plateau:      s.FlatRounds >= s.Stop.Plateau,
		Done:         done,
		DoneReason:   reason,
		FlatRounds:   s.FlatRounds,
		ChangedFiles: r.ChangedFiles,
		Table:        renderTable(s),
	}
}

func checkSlots(s *state, variant string, data variantData) ([]string, error) {
	results := resultCounts(data.Rows)
	if dups := duplicateSlots(results); len(dups) > 0 {
		return nil, fmt.Errorf("%s/results.jsonl has duplicate (case, rep) rows (%s); keep one row per slot and gate again", variant, listed(dups))
	}
	missing, erroredOnly := classifySlots(s, results, data.Errors)
	if len(missing) > 0 {
		return nil, fmt.Errorf("%s is incomplete: %d (case, rep) slots have no result or error row (%s)", variant, len(missing), listed(missing))
	}
	if variant == "baseline" && len(erroredOnly) > 0 {
		return nil, fmt.Errorf("baseline has %d slots that only errored (%s); fix the harness and resume the run until every slot is scored", len(erroredOnly), listed(erroredOnly))
	}
	return erroredOnly, nil
}

func resultCounts(rows []resultRow) map[string]int {
	counts := make(map[string]int, len(rows))
	for _, row := range rows {
		counts[slot(row.CaseID, row.Rep)]++
	}
	return counts
}

func duplicateSlots(counts map[string]int) []string {
	var dups []string
	for key, n := range counts {
		if n > 1 {
			dups = append(dups, key)
		}
	}
	slices.Sort(dups)
	return dups
}

func classifySlots(s *state, results map[string]int, errs []errorRow) (missing, erroredOnly []string) {
	errored := make(map[string]bool, len(errs))
	for _, e := range errs {
		errored[slot(e.CaseID, e.Rep)] = true
	}
	for _, id := range slices.Concat(s.TrainIDs, s.TestIDs) {
		for rep := range s.Reps {
			key := slot(id, rep)
			switch {
			case results[key] > 0:
			case errored[key]:
				erroredOnly = append(erroredOnly, key)
			default:
				missing = append(missing, key)
			}
		}
	}
	return missing, erroredOnly
}

func slot(id string, rep int) string {
	return fmt.Sprintf("%s#%d", id, rep)
}

func checkLeaks(s *state, variant string, data variantData) error {
	if err := checkTraces(s, variant, data); err != nil {
		return err
	}
	return checkTestText(s, data.Rows)
}

func checkTraces(s *state, variant string, data variantData) error {
	test := idSet(s.TestIDs)
	var leaked []string
	for _, name := range data.Traces {
		m := traceName.FindStringSubmatch(name)
		switch {
		case m == nil:
			return fmt.Errorf("%s/traces holds a file that is not <case>_rep<k>.json: %s", variant, name)
		case test[m[1]]:
			leaked = append(leaked, name)
		}
	}
	if len(leaked) > 0 {
		return fmt.Errorf("%s/traces holds transcripts for test cases (%s); write traces for train cases only, delete these, and run again", variant, strings.Join(leaked, ", "))
	}
	return nil
}

func checkTestText(s *state, rows []resultRow) error {
	test := idSet(s.TestIDs)
	for _, row := range rows {
		if !test[row.CaseID] {
			continue
		}
		if key, ok := textField(row); ok {
			return fmt.Errorf("test row %s carries text field %s; keep output text of test cases out of results.jsonl", slot(row.CaseID, row.Rep), key)
		}
	}
	return nil
}

func textField(row resultRow) (string, bool) {
	for _, key := range slices.Sorted(maps.Keys(row.Fields)) {
		if key == "case_id" || key == "status" || key == "model" {
			continue
		}
		if row.Fields[key].Kind() == '"' {
			return key, true
		}
	}
	return "", false
}

func dataWarnings(data variantData, known map[string]bool) []string {
	strays := make(map[string]bool)
	for _, row := range data.Rows {
		if !known[row.CaseID] {
			strays[row.CaseID] = true
		}
	}
	var warnings []string
	if n := len(strays); n > 0 {
		warnings = append(warnings, fmt.Sprintf("%d result ids are outside the split and were ignored", n))
	}
	if n := countTruncated(filterRows(data.Rows, known)); n > 0 {
		warnings = append(warnings, fmt.Sprintf("%d truncated rows excluded from quality means", n))
	}
	if n := len(data.Errors); n > 0 {
		warnings = append(warnings, fmt.Sprintf("%d infra error rows recorded in errors.jsonl", n))
	}
	return warnings
}

func countTruncated(rows []resultRow) int {
	n := 0
	for _, row := range rows {
		if row.Status == "truncated" {
			n++
		}
	}
	return n
}

func changedFiles(previous, current []fileSHA) []string {
	before := make(map[string]string, len(previous))
	for _, f := range previous {
		before[f.Path] = f.SHA
	}
	after := make(map[string]string, len(current))
	for _, f := range current {
		after[f.Path] = f.SHA
	}
	var out []string
	for _, p := range slices.Sorted(maps.Keys(union(before, after))) {
		if before[p] != after[p] {
			out = append(out, p)
		}
	}
	return out
}

func union(a, b map[string]string) map[string]bool {
	keys := make(map[string]bool, len(a)+len(b))
	for key := range a {
		keys[key] = true
	}
	for key := range b {
		keys[key] = true
	}
	return keys
}

func filePaths(files []fileSHA) []string {
	out := make([]string, 0, len(files))
	for _, f := range files {
		out = append(out, f.Path)
	}
	return out
}

func listed(items []string) string {
	shown, suffix := items, ""
	if len(items) > 10 {
		shown, suffix = items[:10], ", …"
	}
	return strings.Join(shown, ", ") + suffix
}

func (s *state) variants() []string {
	var out []string
	for _, r := range s.Rounds {
		if !slices.Contains(out, r.Variant) {
			out = append(out, r.Variant)
		}
	}
	return out
}

func (f *flow) train(s *state, variant string) (trainView, error) {
	if !s.gated(variant) {
		return trainView{}, fmt.Errorf("train needs a gated round for %s", variant)
	}
	data, err := f.readVariant(variant)
	if err != nil {
		return trainView{}, err
	}
	train := idSet(s.TrainIDs)
	view := trainView{
		Variant: variant,
		Rows:    trainRows(data.Rows, train),
		Traces:  f.trainTraces(variant, data.Traces, train),
	}
	view.History, err = f.trainHistory(s, train)
	if err != nil {
		return trainView{}, err
	}
	return view, nil
}

func trainRows(rows []resultRow, train map[string]bool) []jsontext.Value {
	out := []jsontext.Value{}
	for _, row := range rows {
		if train[row.CaseID] {
			out = append(out, row.Raw)
		}
	}
	return out
}

func (f *flow) trainTraces(variant string, names []string, train map[string]bool) []string {
	out := []string{}
	for _, name := range names {
		if m := traceName.FindStringSubmatch(name); m != nil && train[m[1]] {
			out = append(out, filepath.ToSlash(filepath.Join(f.dir, variant, "traces", name)))
		}
	}
	return out
}

func (f *flow) trainHistory(s *state, train map[string]bool) (map[string]map[string]*float64, error) {
	history := make(map[string]map[string]*float64, len(s.TrainIDs))
	for _, id := range s.TrainIDs {
		history[id] = make(map[string]*float64, len(s.Rounds))
	}
	for _, v := range s.variants() {
		data, err := f.readVariant(v)
		if err != nil {
			return nil, err
		}
		means := meansByCase(filterRows(data.Rows, train), s.Goal.Target)
		for _, id := range s.TrainIDs {
			var value *float64
			if mean, ok := means[id]; ok {
				value = new(mean)
			}
			history[id][v] = value
		}
	}
	return history, nil
}

func renderTable(s *state) string {
	target := s.Goal.Target
	cols := []string{"round", "variant", "change", "decision", "train " + target, "test " + target}
	for _, m := range s.Goal.Hold {
		cols = append(cols, "train "+m, "test "+m)
	}
	var b strings.Builder
	fmt.Fprintf(&b, "| %s |\n", strings.Join(cols, " | "))
	fmt.Fprintf(&b, "|%s\n", strings.Repeat("---|", len(cols)))
	for _, r := range s.Rounds {
		cells := []string{fmt.Sprint(r.Round), variantCell(s, r.Variant), strings.ReplaceAll(r.Change, "|", `\|`), r.Decision}
		cells = append(cells, summaryCell(r.Train[target]), summaryCell(r.Test[target]))
		for _, m := range s.Goal.Hold {
			cells = append(cells, summaryCell(r.Train[m]), summaryCell(r.Test[m]))
		}
		fmt.Fprintf(&b, "| %s |\n", strings.Join(cells, " | "))
	}
	return b.String()
}

func variantCell(s *state, variant string) string {
	if s.Best != nil && s.Best.Variant == variant {
		return variant + " (best)"
	}
	return variant
}

func summaryCell(s summary) string {
	switch {
	case s.Mean == nil:
		return "–"
	case s.Half == nil:
		return fmt.Sprintf("%.3f", *s.Mean)
	}
	return fmt.Sprintf("%.3f ±%.3f", *s.Mean, *s.Half)
}
