package agent

import (
	"cmp"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"html"
	"slices"
	"strings"
	"sync"
	"time"

	"golang.org/x/sync/errgroup"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/tool"
)

const (
	reviewTimeout            = 8 * time.Second
	reviewFlagThreshold      = 0.9
	reviewUncertainThreshold = 0.5
	reviewSoundThreshold     = 0.5
	reviewFeedbackCap        = 3
	reviewRequestLimit       = 30000
	reviewMaxParts           = 8
	reviewPartWorkers        = 4
	reviewHostRequests       = 8
	reviewLoadLimit          = 4 << 20
	reviewContextLines       = 3
	soundID                  = "sound"
	reviewClean              = "clean"
	reviewUncertain          = "uncertain"
	reviewFlagged            = "flagged"
	reviewError              = "error"
	reviewPathsWithheld      = "[paths withheld: masking failed]"
	reviewSummaryErrorBytes  = 120
)

var (
	errReviewInterrupted  = errors.New("review: the turn was interrupted")
	errReviewMask         = errors.New("review: the judge state could not be masked")
	errReviewSecret       = errors.New("review: the judge state held an unmasked secret")
	errReviewNoPart       = errors.New("review: no part fits the request limit")
	reviewBuiltInSpeakers = speakersOf(reviewQuestions)
	allKinds              = []string{"code", "interface", "config", "text"}
)

type reviewQuestion struct {
	id, label, feedback, instructions, yes, no string
	kinds                                      []string
	turn, concurrent, needsTask                bool
	speaker                                    bool
}

var reviewQuestions = []reviewQuestion{
	{id: soundID, instructions: "Do the added lines in `files[].diff` serve `task` and show no defect? Judge only the lines shown. A file with `truncated` set to true has lines you cannot see.", yes: "Every added line serves `task`, and no added line shows a defect.", no: "An added line shows a defect, or the diff holds a change that `task` does not ask for.", kinds: allKinds},
	{id: "broken_syntax", label: "broken syntax", feedback: "The added lines may leave a bracket, brace, string, or block unclosed.", instructions: "Do the added lines in `files[].diff` leave the file syntactically broken? Judge only the lines shown. A file with `truncated` set to true has lines you cannot see.", yes: "An added line opens a bracket, brace, parenthesis, string, or block that no shown line closes, or closes one that no shown line opens.", no: "Every bracket, brace, parenthesis, string, and block touched by the added lines opens and closes within the shown lines.", kinds: []string{"code", "interface", "config"}},
	{id: "lost_code", label: "removed code", feedback: "The edit may remove code whose behaviour no added line restores.", instructions: "Does the diff in `files[].diff` remove code whose behaviour no added line restores and that `task` does not ask to remove? Judge only the lines shown.", yes: "A removed line holds a statement, branch, check, function, or test whose behaviour no added line restores, and `task` does not ask for that removal.", no: "Every removed line is rewritten by an added line, appears again elsewhere in the shown diff, or is removed because `task` asks for it.", kinds: allKinds},
	{id: "wrong_logic", label: "wrong logic", feedback: "An added condition, bound, index, return value, or error handling may contradict the lines around it.", instructions: "Does an added line in `files[].diff` contradict the lines shown around it, or drop an error value that it receives? Judge only the lines shown.", yes: "An added line holds a condition, bound, index or return value that contradicts the shown lines around it, or it drops an error value that the same line receives.", no: "Every added condition, bound, index and return value agrees with the shown lines around it, and every received error value is checked or returned.", kinds: []string{"code", "interface"}},
	{id: "placeholder", label: "placeholder", feedback: "An added line may be a placeholder instead of working code.", instructions: "Does an added line in `files[].diff` stand in for code that was not written?", yes: "An added line is a placeholder such as `TODO`, `...`, `pass`, `unimplemented`, an empty function body, or a comment saying existing code stays unchanged.", no: "Every added function body holds working statements, and no added comment says that code was left out.", kinds: allKinds},
	{id: "off_task", label: "off-task change", feedback: "The edit may change code that the task does not ask for.", instructions: "Does the diff in `files[].diff` change code that `task` does not call for?", yes: "A hunk renames, reformats, moves, or rewrites code that neither `task` nor the change `task` asks for needs.", no: "Every hunk changes code that `task` names or that the requested change needs.", kinds: allKinds, needsTask: true},
	{id: "needless_layer", label: "needless layer", feedback: "An added function, type, option, or parameter may only pass its input on, or a block may nest too deeply.", instructions: "Do the added lines in `files[].diff` add a function, type, option, or parameter that does no work of its own, or nest blocks too deeply? Judge only the lines shown.", yes: "The added lines introduce a function, type, option or parameter that the shown lines use in one place and that only passes its input on, or they nest blocks more than three levels deep.", no: "Every added function, type, option and parameter does work of its own or has more than one user in the shown lines, and no added block nests more than three levels deep.", kinds: []string{"code", "interface"}},
	{id: "narration", label: "narration", feedback: "An added comment may repeat the line below it, or an added line may be commented-out code or debug output.", instructions: "Do the added lines in `files[].diff` add comments that describe the change or repeat the line below, or add commented-out code or debug output? Judge only the lines shown.", yes: "An added comment repeats the line below it or describes the change itself, or an added line is commented-out code or leftover debug output.", no: "Every added comment states something the code cannot say, and no added line is commented-out code or debug output.", kinds: []string{"code", "interface"}},
	{id: "unguarded_state", label: "unguarded state", feedback: "An added line may use shared state or start a task without a lock, atomic operation, stop, or wait.", instructions: "Do the added lines in `files[].diff` touch state that two goroutines, threads, or async tasks share, or start a task, without a guard in the lines shown? Judge only the lines shown.", yes: "An added line reads or writes a value that two goroutines, threads or async tasks share, and no shown line guards it; or an added line starts a task that no shown line stops, waits for or cancels.", no: "Every shared value that the added lines touch has a lock, channel or atomic operation in the shown lines, and every started task has a shown stop, wait or cancel.", kinds: []string{"code", "interface"}, concurrent: true},
	{id: "wasted_work", label: "wasted work", feedback: "An added loop may repeat work whose result does not change, or an added read may have no bound.", instructions: "Do the added lines in `files[].diff` repeat work inside a loop, or read input into memory with no bound? Judge only the lines shown.", yes: "An added loop repeats a call, allocation, query or file read whose result is the same in every pass, or an added line reads a whole input into memory with no bound.", no: "Every call inside an added loop depends on the loop's own value, and every added read has a bound in the shown lines.", kinds: []string{"code", "interface"}},
	{id: "unsafe_input", label: "unsafe input", feedback: "An added line may hold a literal credential, or build a command, query, path, or markup from an outside value without binding or escaping it.", instructions: "Do the added lines in `files[].diff` hold a literal credential, or pass an outside value to a command, query, path, or markup without binding or escaping it? Judge only the lines shown.", yes: "An added line holds a literal key, token or password, or it builds a command, query, path or markup from an outside value without the binding or escaping that the shown lines use elsewhere.", no: "No added line holds a literal credential, and every outside value that reaches a command, query, path or markup is bound or escaped in the shown lines.", kinds: []string{"code", "interface", "config"}},
	{id: "unclear_contract", label: "unclear contract", feedback: "An added exported function may choose its behaviour with a flag, or an added name may not say what the code does.", instructions: "Do the added lines in `files[].diff` add an exported function that switches behaviour on a boolean or untyped value, or a name that does not say what the code does? Judge only the lines shown.", yes: "An added exported function takes a boolean or untyped value that selects between behaviours, or an added name (`helper`, `manager`, `util`, `data`, `handle`, `process`) does not say what the code does.", no: "Every added exported function has one behaviour per name, and every added name says what the code does.", kinds: []string{"code", "interface"}},
	{id: "off_token", label: "off-token style", feedback: "An added style may use a literal colour, font, spacing, or radius, add a gradient, or remove a focus outline.", instructions: "Do the added styles in `files[].diff` set literal values where the shown lines use variables, or add a gradient or remove a focus outline? Judge only the lines shown.", yes: "An added style sets a literal colour, font, spacing or radius where the shown lines of that file use variables for that kind of value, or it adds a gradient, or it removes a focus outline.", no: "Every added colour, font, spacing and radius uses a variable where the shown lines do, and no added style adds a gradient or removes a focus outline.", kinds: []string{"interface"}},
	{id: "layout_motion", label: "layout or motion", feedback: "An added style may fix a size in pixels with no narrow-screen rule, or add motion with no reduced-motion rule.", instructions: "Do the added styles in `files[].diff` fix a container's size in pixels or hide overflow with no narrow-screen rule, or add motion with no reduced-motion rule? Judge only the lines shown.", yes: "An added style fixes a container's width or height in pixels or hides overflowing text with no rule for narrow screens in the shown lines, or it adds an animation or transition with no reduced-motion rule, or it transitions `all`.", no: "Every added size bends with its container or has a narrow-screen rule in the shown lines, and every added animation or transition names its properties and has a reduced-motion rule.", kinds: []string{"interface"}},
	{id: "incomplete", label: "incomplete", feedback: "The task may name a change that the diff does not make, or leave a placeholder where the task needs working code.", instructions: "Does the diff in `files[].diff` leave a change that `task` names undone, or leave a placeholder where `task` needs working code? Judge only the lines shown.", yes: "`task` names a change, file, case or test that no hunk of the diff makes, or a hunk leaves a placeholder where `task` needs working code.", no: "Every change that `task` names appears in the diff as working code.", kinds: allKinds, turn: true, needsTask: true},
}

func speakersOf(table []reviewQuestion) []string {
	var out []string
	for _, q := range table {
		if q.speaker {
			out = append(out, q.id)
		}
	}
	return out
}

type reviewRound struct {
	group   sync.WaitGroup
	pending []*event.ToolJudged
}

type judgment struct {
	by      string
	scores  map[string]float64
	verdict string
	flags   []string
	spoken  []string
	err     error
	failed  int
}

func checkReviewFeedback(ids []string) error {
	for _, id := range ids {
		if id == soundID || !slices.ContainsFunc(reviewQuestions, func(q reviewQuestion) bool { return q.id == id }) {
			return fmt.Errorf("edit_review.feedback: unknown question %s", id)
		}
	}
	return nil
}

func (h *Host) maskReview(text string) (string, error) {
	if h.reviewMask != nil {
		return h.reviewMask(text)
	}
	return h.mask(text)
}

func (w *worker) startReview(ctx context.Context, p *prepared, completed event.ToolCompleted) {
	if w.round == nil || w.t.host.cfg.EditJudge == nil || !w.t.host.Config().EditReview.Enabled || p.class != tool.ClassEdit || !completed.OK || len(p.files) == 0 {
		return
	}
	task := w.task()
	files := slices.Clone(p.files)
	name, callID := p.call.Name, p.call.ID
	slot := new(event.ToolJudged)
	w.round.pending = append(w.round.pending, slot)
	w.round.group.Go(func() { *slot = w.judgeEdit(ctx, callID, task, name, files) })
}

func (w *worker) joinReviews(ctx context.Context) error {
	round := w.round
	w.round = nil
	if round == nil {
		return nil
	}
	round.group.Wait()
	given, err := w.feedbackGiven()
	if err != nil {
		return err
	}
	var judged []event.Event
	for _, slot := range round.pending {
		if slot.CallID == "" {
			continue
		}
		w.eligible = true
		entry := *slot
		entry.Feedback = entry.Verdict == reviewFlagged && len(entry.Spoken) > 0 && given < reviewFeedbackCap
		if entry.Feedback {
			given++
		} else {
			entry.Spoken = nil
		}
		judged = append(judged, entry)
	}
	if len(judged) == 0 {
		return nil
	}
	_, err = w.t.appendEvents(context.WithoutCancel(ctx), judged...)
	return err
}

func (w *worker) feedbackGiven() (int, error) {
	v, err := w.t.view()
	if err != nil {
		return 0, err
	}
	given := 0
	for _, item := range event.ActivePath(v.records) {
		if judged, ok := item.Event.(event.ToolJudged); ok && judged.Feedback && judged.TurnID == w.turnID {
			given++
		}
	}
	return given, nil
}

func (w *worker) judgeEdit(ctx context.Context, callID, task, name string, changes []event.FileChange) event.ToolJudged {
	h := w.t.host
	tctx, cancel := context.WithTimeoutCause(ctx, h.reviewTimeout, reviewTimeoutCause(h.reviewTimeout))
	defer cancel()
	start := time.Now()
	unit := w.prepareUnit(tctx, task, name, changes)
	if unit.err == nil && unit.added == 0 {
		return event.ToolJudged{}
	}
	if unit.err == nil && len(unit.parts) == 0 {
		unit.err = errReviewNoPart
	}
	if unit.err != nil {
		return w.toolJudged(callID, unit, judgment{by: "judge", err: unit.err}, 0, start)
	}
	j, requests := w.judgeParts(ctx, tctx, unit.parts)
	return w.toolJudged(callID, unit, j, requests, start)
}

func reviewTimeoutCause(d time.Duration) error {
	return fmt.Errorf("review timed out after %s", d)
}

func reviewFeedback(e event.ToolJudged) string {
	paths := make([]string, 0, len(e.Paths))
	for _, path := range e.Paths {
		paths = append(paths, html.EscapeString(path))
	}
	coverage := ""
	if e.Truncated {
		coverage = "This review covered part of the edit.\n"
	}
	return "<edit-review call=\"" + html.EscapeString(e.CallID) + "\" paths=\"" + strings.Join(paths, ", ") + "\">\n" +
		"An automatic review of this edit found possible problems:\n" + reviewBullets(e.Spoken) + coverage +
		"Check the changed lines. If a problem is real, fix it. If not, ignore this review.\n</edit-review>"
}

func reviewTurnFeedback(e event.TurnJudged) string {
	return "<turn-review turn=\"" + html.EscapeString(e.TurnID) + "\">\n" +
		"An automatic review of this turn found possible problems:\n" + reviewBullets(e.Spoken) +
		"Check the changed files. If a problem is real, fix it. If not, ignore this review.\n</turn-review>"
}

func reviewBullets(spoken []string) string {
	var out strings.Builder
	for _, id := range spoken {
		out.WriteString("- " + reviewSentence(id) + "\n")
	}
	return out.String()
}

func reviewSentence(id string) string {
	for _, q := range reviewQuestions {
		if q.id == id {
			return q.feedback
		}
	}
	return "The review raised the flag " + id + "."
}

type ReviewOutcome struct {
	Verdict   string
	Scores    map[string]float64
	Flags     []string
	Spoken    []string
	Error     string
	Truncated bool
}

func ReviewSummary(o ReviewOutcome) string {
	switch o.Verdict {
	case reviewClean:
		return "No problems found"
	case reviewFlagged:
		text := "Flagged: " + reviewLabels(o.Flags)
		if len(o.Spoken) == 0 {
			text += " (not sent to the model)"
		}
		return text + reviewCoverage(o.Truncated)
	case reviewUncertain:
		return reviewUncertainSummary(o.Scores, o.Truncated)
	case reviewError:
		return "Failed: " + truncate(o.Error, reviewSummaryErrorBytes)
	}
	return ""
}

func reviewUncertainSummary(scores map[string]float64, truncated bool) string {
	var labels []string
	for _, q := range reviewQuestions {
		score, ok := scores[q.id]
		if q.id != soundID && ok && score >= reviewUncertainThreshold && score < reviewFlagThreshold {
			labels = append(labels, q.label)
		}
	}
	if len(labels) == 0 {
		if truncated {
			return "Uncertain: part of the diff was not judged"
		}
		return "Uncertain: no label applies"
	}
	return "Uncertain: " + strings.Join(labels, ", ") + reviewCoverage(truncated)
}

func reviewCoverage(truncated bool) string {
	if truncated {
		return "; part of the diff was not judged"
	}
	return ""
}

func reviewLabels(ids []string) string {
	labels := make([]string, 0, len(ids))
	for _, id := range ids {
		labels = append(labels, reviewLabel(id))
	}
	return strings.Join(labels, ", ")
}

func reviewLabel(id string) string {
	for _, q := range reviewQuestions {
		if q.id == id {
			return q.label
		}
	}
	return id
}

func (w *worker) prepareUnit(ctx context.Context, task, name string, changes []event.FileChange) unitInput {
	h := w.t.host
	paths, err := maskReviewPaths(h, changes)
	if err != nil {
		return unitInput{paths: paths, err: err}
	}
	files, unit, err := w.loadUnitFiles(ctx, changes, paths)
	if err != nil {
		return unitInput{paths: paths, err: err}
	}
	diffs, err := diffFiles(files, reviewContextLines)
	if err != nil {
		return unitInput{paths: paths, err: err}
	}
	parts, omitted, err := packMasked(h.maskReview, task, name, diffs)
	if err != nil {
		return unitInput{paths: paths, err: err}
	}
	if omitted > 0 {
		unit.truncated = true
	}
	for _, d := range diffs {
		unit.added += d.added
	}
	unit.parts, unit.paths = parts, paths
	return unit
}

func maskReviewPaths(h *Host, changes []event.FileChange) ([]string, error) {
	paths := make([]string, 0, len(changes))
	for _, change := range changes {
		masked, err := h.maskReview(change.Path)
		if err != nil {
			return []string{reviewPathsWithheld}, errReviewMask
		}
		paths = append(paths, masked)
	}
	return paths, nil
}

func packMasked(mask func(string) (string, error), task, name string, diffs []fileDiff) ([]judgeInput, int, error) {
	parts, omitted, err := packParts(task, name, diffs)
	if err != nil {
		return nil, 0, err
	}
	for _, part := range parts {
		if err := requireMasked(mask, part.state); err != nil {
			return nil, 0, err
		}
	}
	return parts, omitted, nil
}

func buildReview(task, name string, files []reviewFile) ([]judgeInput, int, error) {
	diffs, err := diffFiles(files, reviewContextLines)
	if err != nil {
		return nil, 0, err
	}
	return packParts(task, name, diffs)
}

func (w *worker) loadUnitFiles(ctx context.Context, changes []event.FileChange, paths []string) ([]reviewFile, unitInput, error) {
	var unit unitInput
	files := make([]reviewFile, 0, len(changes))
	loaded := 0
	for i, change := range changes {
		file, err := w.loadReviewFile(ctx, change, paths[i])
		if err != nil {
			return nil, unit, err
		}
		files = append(files, file)
		loaded += len(file.pre) + len(file.post)
		if loaded >= reviewLoadLimit && i+1 < len(changes) {
			unit.truncated = true
			break
		}
	}
	for _, file := range files {
		if file.skip == "large" {
			unit.truncated = true
		}
	}
	return files, unit, nil
}

func (w *worker) loadReviewFile(ctx context.Context, change event.FileChange, path string) (reviewFile, error) {
	h := w.t.host
	var pre, post []byte
	if change.PreBlob != nil {
		data, err := h.journal.Blob(change.PreBlob.Blob)
		if err != nil {
			return reviewFile{}, fmt.Errorf("review: read pre-image of %s: %w", path, err)
		}
		pre = data
	}
	if change.PostSHA != "" {
		raw, err := w.t.runner.ReadFile(ctx, absPath(w.t.created.Cwd, change.Path))
		if err != nil {
			return reviewFile{}, fmt.Errorf("review: read %s: %w", path, err)
		}
		sum := sha256.Sum256(raw)
		if hex.EncodeToString(sum[:]) != change.PostSHA {
			return reviewFile{}, fmt.Errorf("review: %s changed before it was judged", path)
		}
		masked, err := h.maskReview(string(raw))
		if err != nil {
			return reviewFile{}, errReviewMask
		}
		post = []byte(masked)
	}
	return classifyReviewFile(path, reviewChange(change), pre, post), nil
}

func reviewChange(change event.FileChange) string {
	switch {
	case change.PreSHA == "":
		return "add"
	case change.PostSHA == "":
		return "delete"
	}
	return "update"
}

func (w *worker) judgeParts(ctx, tctx context.Context, parts []judgeInput) (judgment, int) {
	client, name, err := w.t.host.cfg.EditJudge(tctx, w.t.id)
	if err != nil {
		return judgment{by: "judge", err: reviewFailure(ctx, tctx, err)}, 0
	}
	by := "judge:" + name
	results := make([]judgment, len(parts))
	var group errgroup.Group
	group.SetLimit(reviewPartWorkers)
	for i, part := range parts {
		group.Go(func() error {
			results[i] = w.askPart(ctx, tctx, client, by, part)
			return nil
		})
	}
	_ = group.Wait()
	return combineParts(results), len(parts)
}

func (w *worker) askPart(ctx, tctx context.Context, client decide.Client, by string, part judgeInput) judgment {
	h := w.t.host
	select {
	case h.reviewSlots <- struct{}{}:
	case <-tctx.Done():
		return judgment{by: by, err: reviewFailure(ctx, tctx, nil)}
	}
	defer func() { <-h.reviewSlots }()
	answers, err := client.Ask(tctx, part.state, part.questions)
	if err != nil {
		return judgment{by: by, err: reviewFailure(ctx, tctx, err)}
	}
	scores, err := scoreAnswers(answers, part.questions)
	if err != nil {
		return judgment{by: by, err: err}
	}
	verdict, flags := reviewVerdict(scores)
	return judgment{by: by, scores: scores, verdict: verdict, flags: flags, spoken: reviewSpoken(scores, flags, h.cfg.EditFeedback)}
}

func reviewFailure(ctx, tctx context.Context, err error) error {
	switch {
	case ctx.Err() != nil:
		return errReviewInterrupted
	case tctx.Err() != nil:
		return context.Cause(tctx)
	}
	return err
}

func reviewVerdict(scores map[string]float64) (string, []string) {
	var flags []string
	uncertain := false
	for _, q := range reviewQuestions {
		score, ok := scores[q.id]
		if !ok || q.id == soundID {
			continue
		}
		switch {
		case score >= reviewFlagThreshold:
			flags = append(flags, q.id)
		case score >= reviewUncertainThreshold:
			uncertain = true
		}
	}
	switch {
	case len(flags) > 0:
		return reviewFlagged, flags
	case uncertain:
		return reviewUncertain, nil
	}
	return reviewClean, nil
}

func reviewSpoken(scores map[string]float64, flags, feedback []string) []string {
	sound, ok := scores[soundID]
	if !ok || sound >= reviewSoundThreshold {
		return nil
	}
	speakers := feedback
	if speakers == nil {
		speakers = reviewBuiltInSpeakers
	}
	var spoken []string
	for _, flag := range flags {
		if slices.Contains(speakers, flag) {
			spoken = append(spoken, flag)
		}
	}
	return spoken
}

func combineParts(parts []judgment) judgment {
	out := judgment{by: parts[0].by}
	var answered []judgment
	var firstErr error
	for i, part := range parts {
		if part.err != nil {
			out.failed++
			if firstErr == nil {
				firstErr = fmt.Errorf("part %d of %d: %w", i+1, len(parts), part.err)
			}
			continue
		}
		answered = append(answered, part)
	}
	if len(answered) == 0 {
		out.err = firstErr
		return out
	}
	out.verdict = worstVerdict(answered)
	if out.failed > 0 && out.verdict == reviewClean {
		out.verdict = reviewUncertain
	}
	out.scores = mergeScores(answered)
	out.flags = unionOrdered(answered, func(j judgment) []string { return j.flags })
	out.spoken = unionOrdered(answered, func(j judgment) []string { return j.spoken })
	return out
}

func worstVerdict(parts []judgment) string {
	worst := reviewClean
	for _, part := range parts {
		switch part.verdict {
		case reviewFlagged:
			return reviewFlagged
		case reviewUncertain:
			worst = reviewUncertain
		}
	}
	return worst
}

func mergeScores(parts []judgment) map[string]float64 {
	out := map[string]float64{}
	for _, part := range parts {
		for name, score := range part.scores {
			prev, seen := out[name]
			switch {
			case !seen:
				out[name] = score
			case name == soundID:
				out[name] = min(prev, score)
			default:
				out[name] = max(prev, score)
			}
		}
	}
	return out
}

func unionOrdered(parts []judgment, pick func(judgment) []string) []string {
	seen := map[string]bool{}
	for _, part := range parts {
		for _, name := range pick(part) {
			seen[name] = true
		}
	}
	var out []string
	for _, q := range reviewQuestions {
		if seen[q.id] {
			out = append(out, q.id)
		}
	}
	return out
}

func (w *worker) toolJudged(callID string, unit unitInput, j judgment, requests int, start time.Time) event.ToolJudged {
	entry := event.ToolJudged{
		CallID: callID, TurnID: w.turnID, By: j.by, Paths: capPaths(unit.paths), Parts: cmp.Or(requests, 1),
		Truncated: unit.truncated || j.failed > 0, ElapsedMS: time.Since(start).Milliseconds(),
	}
	if j.failed > 0 {
		entry.FailedParts = j.failed
	}
	if j.err != nil {
		entry.Verdict = reviewError
		entry.Error = w.reviewErrorText(j.err)
		return entry
	}
	entry.Verdict, entry.Scores, entry.Flags, entry.Spoken = j.verdict, j.scores, j.flags, j.spoken
	if entry.Truncated && entry.Verdict == reviewClean {
		entry.Verdict = reviewUncertain
	}
	return entry
}

func capPaths(paths []string) []string {
	if len(paths) > event.MaxJudgedPaths {
		return paths[:event.MaxJudgedPaths]
	}
	return paths
}

func (w *worker) reviewErrorText(err error) string {
	text, maskErr := w.t.host.maskReview(truncate(err.Error(), event.MaxJudgeErrorBytes))
	if maskErr != nil {
		return "review: the error text could not be masked"
	}
	return text
}
