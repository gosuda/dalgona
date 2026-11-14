package agent

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"strconv"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/tool"
)

type turnDiff struct {
	paths, skipped []string
	files          []reviewFile
	truncated      bool
	err            error
}

type turnState struct {
	Task  string      `json:"task,omitempty"`
	Files []judgeFile `json:"files"`
}

var errReviewTurnSize = errors.New("review: turn state exceeds the request limit")

func (w *worker) turnReview(ctx context.Context, v view) (bool, uint64) {
	h := w.t.host
	task, wanted := w.turnReviewWanted(v)
	if !wanted {
		return false, v.head()
	}
	w.reviewed = true
	start := time.Now()
	judged, ok := w.judgeTurn(ctx, task, w.collectTurnDiff(v.records), start)
	if !ok {
		return false, v.head()
	}
	head, err := w.t.appendEvents(context.WithoutCancel(ctx), judged)
	if err != nil {
		h.log.Warn("agent: journal turn.judged", "thread", w.t.id, "turn", w.turnID, "error", err)
		return ctx.Err() != nil, v.head()
	}
	return judged.Feedback || ctx.Err() != nil, head
}

func (w *worker) turnReviewWanted(v view) (string, bool) {
	if w.t.host.cfg.EditJudge == nil || !w.t.host.Config().EditReview.Enabled || !w.eligible || w.recovering || w.reviewed {
		return "", false
	}
	if _, judged := v.state.JudgedTurns[w.turnID]; judged {
		return "", false
	}
	task := w.task()
	return task, task != ""
}

func (w *worker) collectTurnDiff(records []event.Seqd) turnDiff {
	h := w.t.host
	order, first, last := w.turnChanges(event.ActivePath(records))
	masked := make([]string, 0, len(order))
	for _, raw := range order {
		text, err := h.mask(raw)
		if err != nil {
			return turnDiff{paths: []string{reviewPathsWithheld}, err: errReviewMask}
		}
		masked = append(masked, text)
	}
	var diff turnDiff
	skip := make([]bool, len(order))
	for i, raw := range order {
		end := last[raw]
		if end.PostSHA != "" && end.PostBlob == nil {
			skip[i] = true
			continue
		}
		file, err := w.turnFile(first[raw], end, masked[i])
		if err != nil {
			return turnDiff{paths: capPaths(masked), err: err}
		}
		if file != nil {
			diff.truncated = diff.truncated || file.skip == "large"
			diff.files = append(diff.files, *file)
		}
	}
	diff.paths = capPaths(masked)
	diff.skipped = skippedPaths(diff.paths, masked, skip)
	return diff
}

func skippedPaths(paths, masked []string, skip []bool) []string {
	var out []string
	for i := range paths {
		if skip[i] {
			out = append(out, masked[i])
		}
	}
	return out
}

func doneCalls(path []event.Seqd) map[string]bool {
	done := map[string]bool{}
	for _, item := range path {
		if completed, ok := item.Event.(event.ToolCompleted); ok && completed.OK {
			done[completed.CallID] = true
		}
	}
	return done
}

func (w *worker) countedEdit(started event.ToolStarted, done map[string]bool) bool {
	return started.TurnID == w.turnID && started.Class == classString(tool.ClassEdit) && done[started.CallID]
}

func (w *worker) turnChanges(path []event.Seqd) ([]string, map[string]event.FileChange, map[string]event.FileChange) {
	done := doneCalls(path)
	var order []string
	first := map[string]event.FileChange{}
	last := map[string]event.FileChange{}
	for _, item := range path {
		started, ok := item.Event.(event.ToolStarted)
		if !ok || !w.countedEdit(started, done) {
			continue
		}
		for _, change := range started.Files {
			if _, seen := first[change.Path]; !seen {
				first[change.Path] = change
				order = append(order, change.Path)
			}
			last[change.Path] = change
		}
	}
	return order, first, last
}

func (w *worker) turnFile(first, end event.FileChange, path string) (*reviewFile, error) {
	h := w.t.host
	var pre, post []byte
	if first.PreBlob != nil {
		data, err := h.journal.Blob(first.PreBlob.Blob)
		if err != nil {
			return nil, fmt.Errorf("review: read pre-image of %s: %w", path, err)
		}
		pre = data
	}
	if end.PostBlob != nil {
		data, err := h.journal.Blob(end.PostBlob.Blob)
		if err != nil {
			return nil, fmt.Errorf("review: read post-image of %s: %w", path, err)
		}
		post = data
	}
	if bytes.Equal(pre, post) {
		return nil, nil
	}
	file := classifyReviewFile(path, reviewChange(event.FileChange{PreSHA: first.PreSHA, PostSHA: end.PostSHA}), pre, post)
	return &file, nil
}

func (w *worker) judgeTurn(ctx context.Context, task string, diff turnDiff, start time.Time) (event.TurnJudged, bool) {
	h := w.t.host
	entry := event.TurnJudged{TurnID: w.turnID, Paths: diff.paths, Skipped: diff.skipped, Truncated: diff.truncated}
	if diff.err != nil {
		return w.turnResult(entry, judgment{by: "judge", err: diff.err}, start), true
	}
	diffs, err := diffFiles(diff.files, 0)
	if err != nil {
		return w.turnResult(entry, judgment{by: "judge", err: err}, start), true
	}
	input, added, err := turnRequest(task, diffs)
	if err != nil {
		return w.turnResult(entry, judgment{by: "judge", err: err}, start), true
	}
	if added == 0 {
		return event.TurnJudged{}, false
	}
	if err := requireMasked(h.maskReview, input.state); err != nil {
		return w.turnResult(entry, judgment{by: "judge", err: err}, start), true
	}
	entry.Truncated = entry.Truncated || input.truncated
	tctx, cancel := context.WithTimeoutCause(ctx, h.reviewTimeout, reviewTimeoutCause(h.reviewTimeout))
	defer cancel()
	j, _ := w.judgeParts(ctx, tctx, []judgeInput{input})
	return w.turnResult(entry, j, start), true
}

func (w *worker) turnResult(entry event.TurnJudged, j judgment, start time.Time) event.TurnJudged {
	entry.By = j.by
	entry.ElapsedMS = time.Since(start).Milliseconds()
	if j.err != nil {
		entry.Verdict = reviewError
		entry.Error = w.reviewErrorText(j.err)
		return entry
	}
	entry.Verdict, entry.Scores, entry.Flags = j.verdict, j.scores, j.flags
	if entry.Truncated && entry.Verdict == reviewClean {
		entry.Verdict = reviewUncertain
	}
	if !entry.Truncated {
		entry.Spoken = j.spoken
		entry.Feedback = len(j.spoken) > 0
	}
	return entry
}

func turnQuestionSet() map[string]decide.Question {
	set := map[string]decide.Question{}
	for _, q := range reviewQuestions {
		if q.id == soundID || q.turn {
			set[q.id] = decide.Question{Type: decide.TypeNoul, Instructions: q.instructions, Criteria: map[string]string{"true": q.yes, "false": q.no}}
		}
	}
	return set
}

func turnBody(task string, files []judgeFile) (jsontext.Value, error) {
	raw, err := json.Marshal(turnState{Task: task, Files: files})
	return jsontext.Value(raw), err
}

func turnRequest(task string, files []fileDiff) (judgeInput, int, error) {
	added := 0
	base := make([]judgeFile, 0, len(files))
	texts := make([]string, 0, len(files))
	for i := range files {
		f := &files[i]
		added += f.added
		base = append(base, judgeFile{Path: f.path, Kind: f.kind, Change: f.change, Added: f.added, Removed: f.removed, Truncated: false})
		texts = append(texts, fileText(f, f.hunks))
	}
	questions := turnQuestionSet()
	room, err := turnRoom(task, base, questions)
	if err != nil {
		return judgeInput{}, 0, err
	}
	out, truncated := cutJudgeFiles(files, texts, shareRoom(texts, room))
	body, err := turnFits(task, out, questions)
	if err != nil {
		return judgeInput{}, 0, err
	}
	return judgeInput{state: body, questions: questions, truncated: truncated}, added, nil
}

func turnRoom(task string, base []judgeFile, questions map[string]decide.Question) (int, error) {
	body, err := turnBody(task, base)
	if err != nil {
		return 0, err
	}
	size, err := decide.RequestSize(body, questions)
	if err != nil {
		return 0, err
	}
	room := reviewRequestLimit - size
	if room < 0 {
		return 0, errReviewTurnSize
	}
	return room, nil
}

func cutJudgeFiles(files []fileDiff, texts, cut []string) ([]judgeFile, bool) {
	out := make([]judgeFile, 0, len(files))
	truncated := false
	for i, f := range files {
		cutHere := cut[i] != texts[i]
		truncated = truncated || cutHere
		out = append(out, judgeFile{Path: f.path, Kind: f.kind, Change: f.change, Added: f.added, Removed: f.removed, Diff: cut[i], Truncated: cutHere})
	}
	return out, truncated
}

func turnFits(task string, files []judgeFile, questions map[string]decide.Question) (jsontext.Value, error) {
	body, err := turnBody(task, files)
	if err != nil {
		return nil, err
	}
	size, err := decide.RequestSize(body, questions)
	if err != nil {
		return nil, err
	}
	if size > reviewRequestLimit {
		return nil, errReviewTurnSize
	}
	return body, nil
}

func shareRoom(diffs []string, room int) []string {
	out := make([]string, len(diffs))
	left := room
	for i, diff := range diffs {
		share := left / (len(diffs) - i)
		if jsonContentLen(diff) <= share {
			out[i] = diff
		} else {
			out[i] = truncateDiff(diff, share)
		}
		left -= jsonContentLen(out[i])
	}
	return out
}

func truncateDiff(diff string, limit int) string {
	if jsonContentLen(diff) <= limit {
		return diff
	}
	for end := min(len(diff), limit) - 1; end >= 0; end-- {
		if diff[end] != '\n' {
			continue
		}
		kept := diff[:end+1]
		if text := kept + truncMarker(len(diff)-len(kept)); jsonContentLen(text) <= limit {
			return text
		}
	}
	if marker := truncMarker(len(diff)); jsonContentLen(marker) <= limit {
		return marker
	}
	return ""
}

func truncMarker(removed int) string {
	return "[truncated: " + strconv.Itoa(removed) + " more bytes of this diff]\n"
}

func jsonContentLen(s string) int {
	quoted, err := jsontext.AppendQuote(nil, s)
	if err != nil {
		return reviewRequestLimit + 1
	}
	return len(quoted) - 2
}
