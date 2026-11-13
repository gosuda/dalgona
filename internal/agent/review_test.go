package agent

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"maps"
	"math/rand/v2"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/textdiff"
)

type judgeStub struct {
	mu       sync.Mutex
	scores   map[string]float64
	omit     string
	failWhen string
	err      error
	block    bool
	hold     time.Duration
	flight   int
	peak     int
	states   []string
	asked    [][]string
}

func (j *judgeStub) Ask(ctx context.Context, state jsontext.Value, questions map[string]decide.Question) (map[string]decide.Answer, error) {
	ids := slices.Sorted(maps.Keys(questions))
	j.mu.Lock()
	j.states = append(j.states, string(state))
	j.asked = append(j.asked, ids)
	j.mu.Unlock()
	j.mu.Lock()
	j.flight++
	j.peak = max(j.peak, j.flight)
	j.mu.Unlock()
	defer func() {
		j.mu.Lock()
		j.flight--
		j.mu.Unlock()
	}()
	if j.hold > 0 {
		time.Sleep(j.hold)
	}
	if j.block {
		<-ctx.Done()
		return nil, ctx.Err()
	}
	if j.err != nil {
		return nil, j.err
	}
	if j.failWhen != "" && strings.Contains(string(state), j.failWhen) {
		return nil, errors.New("boom")
	}
	answers := make(map[string]decide.Answer, len(ids))
	for _, id := range ids {
		if id == j.omit {
			continue
		}
		score, ok := j.scores[id]
		if !ok {
			score = 0.02
		}
		answers[id] = decide.Answer{Noul: &score}
	}
	return answers, nil
}

func (j *judgeStub) count() int {
	j.mu.Lock()
	defer j.mu.Unlock()
	return len(j.states)
}

func judgeFor(j *judgeStub) ClientFactory {
	return func(context.Context, string) (decide.Client, string, error) { return j, "stub-judge", nil }
}

func judgedHarness(t *testing.T, j *judgeStub, feedback []string) *harness {
	return newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.EditJudge = judgeFor(j)
		h.cfg.Config.EditReview.Enabled = true
		h.cfg.EditFeedback = feedback
	})
}

func patchArgs(path string, pairs ...[2]string) string {
	changes := make([]string, 0, len(pairs))
	for _, p := range pairs {
		changes = append(changes, fmt.Sprintf(`{"path":%q,"old":%q,"new":%q}`, path, p[0], p[1]))
	}
	return `{"changes":[` + strings.Join(changes, ",") + `]}`
}

func runRounds(t *testing.T, h *harness, rounds ...[]call) *Thread {
	t.Helper()
	responses := make([]response, 0, len(rounds)+1)
	for _, round := range rounds {
		responses = append(responses, calls(round...))
	}
	responses = append(responses, text("done"))
	h.model.push(responses...)
	thread := h.start()
	h.submit(thread, "fix it")
	h.waitIdle(thread)
	return thread
}

func judgedOf(events []event.Seqd) []event.ToolJudged {
	var out []event.ToolJudged
	for _, item := range events {
		if judged, ok := item.Event.(event.ToolJudged); ok {
			out = append(out, judged)
		}
	}
	return out
}

func completedIndex(events []event.Seqd, callID string) int {
	for i, item := range events {
		if done, ok := item.Event.(event.ToolCompleted); ok && done.CallID == callID {
			return i
		}
	}
	return -1
}

func TestEditReviewJournalsVerdictAfterCompletion(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{scores: map[string]float64{"sound": 0.2, "lost_code": 0.94}}
	h := judgedHarness(t, judge, []string{"lost_code"})
	h.write("a.txt", "one\ntwo\n")
	thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
	events := h.events(thread.ID())
	judged := judgedOf(events)
	if len(judged) != 1 {
		t.Fatalf("tool.judged count = %d, want 1", len(judged))
	}
	got := judged[0]
	if got.Verdict != reviewFlagged || !slices.Equal(got.Flags, []string{"lost_code"}) || !slices.Equal(got.Spoken, []string{"lost_code"}) || !got.Feedback {
		t.Fatalf("verdict = %+v, want flagged lost_code spoken with feedback", got)
	}
	if got.By != "judge:stub-judge" || !slices.Equal(got.Paths, []string{"a.txt"}) {
		t.Fatalf("by = %q paths = %v", got.By, got.Paths)
	}
	if !judgedAfterCompletion(events, "e1") {
		t.Fatalf("tool.judged is not after tool.completed: %v", h.types(thread.ID()))
	}
}

func judgedAfterCompletion(events []event.Seqd, callID string) bool {
	index := completedIndex(events, callID)
	judged := slices.IndexFunc(events, func(s event.Seqd) bool {
		_, ok := s.Event.(event.ToolJudged)
		return ok
	})
	return index >= 0 && index < judged
}

func requireCompletedOK(t *testing.T, events []event.Seqd, callID string) {
	t.Helper()
	index := completedIndex(events, callID)
	if index < 0 || !events[index].Event.(event.ToolCompleted).OK {
		t.Fatalf("tool.completed for %s is missing or not OK", callID)
	}
}

func TestEditReviewCleanVerdictHasNoFeedback(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{scores: map[string]float64{"sound": 0.02}}
	h := judgedHarness(t, judge, []string{"lost_code"})
	h.write("a.txt", "one\ntwo\n")
	thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
	judged := judgedOf(h.events(thread.ID()))
	if len(judged) != 1 || judged[0].Verdict != reviewClean || judged[0].Feedback || judged[0].Spoken != nil {
		t.Fatalf("judged = %+v, want one clean verdict without feedback", judged)
	}
}

func TestEditReviewFailsOpen(t *testing.T) {
	t.Parallel()
	rows := []struct {
		name    string
		factory ClientFactory
		timeout time.Duration
		want    string
	}{
		{name: "factory error", factory: func(context.Context, string) (decide.Client, string, error) {
			return nil, "", errors.New("no credentials")
		}, want: "no credentials"},
		{name: "ask error", factory: judgeFor(&judgeStub{err: errors.New("boom")}), want: "part 1 of 1: boom"},
		{name: "ask blocks past the limit", factory: judgeFor(&judgeStub{block: true}), timeout: 50 * time.Millisecond, want: "part 1 of 1: review timed out after 50ms"},
		{name: "factory blocks past the limit", factory: func(ctx context.Context, _ string) (decide.Client, string, error) {
			<-ctx.Done()
			return nil, "", ctx.Err()
		}, timeout: 50 * time.Millisecond, want: "review timed out after 50ms"},
	}
	for _, row := range rows {
		t.Run(row.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
					h.cfg.EditJudge = row.factory
					h.cfg.Config.EditReview.Enabled = true
				})
				if row.timeout > 0 {
					h.host.reviewTimeout = row.timeout
				}
				h.write("a.txt", "one\ntwo\n")
				thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
				events := h.events(thread.ID())
				judged := judgedOf(events)
				if len(judged) != 1 || judged[0].Verdict != reviewError || !strings.Contains(judged[0].Error, row.want) {
					t.Fatalf("judged = %+v, want error containing %q", judged, row.want)
				}
				if done := events[completedIndex(events, "e1")].Event.(event.ToolCompleted); !done.OK {
					t.Fatalf("tool.completed.OK = false, want the edit to stand")
				}
			})
		})
	}
}

func TestEditReviewCapsFeedbackPerTurn(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{scores: map[string]float64{"sound": 0.2, "lost_code": 0.94}}
	h := judgedHarness(t, judge, []string{"lost_code"})
	h.write("a.txt", "one\ntwo\n")
	thread := runRounds(t, h,
		[]call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}},
		[]call{{id: "e2", name: "patch", args: patchArgs("a.txt", [2]string{"uno", "una"})}},
		[]call{{id: "e3", name: "patch", args: patchArgs("a.txt", [2]string{"una", "unu"})}},
		[]call{{id: "e4", name: "patch", args: patchArgs("a.txt", [2]string{"unu", "unx"})}},
	)
	judged := judgedOf(h.events(thread.ID()))
	if len(judged) != 4 {
		t.Fatalf("judged count = %d, want 4", len(judged))
	}
	for i, want := range []bool{true, true, true, false} {
		if judged[i].Feedback != want {
			t.Fatalf("judged[%d].Feedback = %v, want %v", i, judged[i].Feedback, want)
		}
	}
	if judged[3].Spoken != nil {
		t.Fatalf("fourth spoken = %v, want nil", judged[3].Spoken)
	}
}

func TestEditReviewSkipsChangeWithoutAddedLines(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.write("a.txt", "one\ntwo\n")
	thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one\ntwo", "two"})}})
	if judge.count() != 0 || len(judgedOf(h.events(thread.ID()))) != 0 {
		t.Fatalf("judge calls = %d, want none for a removal-only edit", judge.count())
	}
}

func TestEditReviewStateNeverHoldsRawSecret(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.host.reviewMask = func(text string) (string, error) {
		return strings.ReplaceAll(text, "SECRETVALUE", "{{secret:TOKEN}}"), nil
	}
	h.write("a.txt", "one\n")
	runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "SECRETVALUE"})}})
	edits := editStatesOf(judge)
	if len(edits) != 1 {
		t.Fatalf("edit judge states = %d, want 1", len(edits))
	}
	judge.mu.Lock()
	states := slices.Clone(judge.states)
	judge.mu.Unlock()
	for _, state := range states {
		if strings.Contains(state, "SECRETVALUE") {
			t.Fatalf("judge state = %s, want no raw secret in any state", state)
		}
	}
	if !strings.Contains(edits[0], "{{secret:TOKEN}}") {
		t.Fatalf("edit judge state = %s, want the placeholder", edits[0])
	}
}

func TestEditReviewSeesEachEditOfSameFile(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.write("a.txt", "one\ntwo\n")
	runRounds(t, h,
		[]call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}},
		[]call{{id: "e2", name: "patch", args: patchArgs("a.txt", [2]string{"uno", "una"})}},
	)
	states := editStatesOf(judge)
	if len(states) != 2 {
		t.Fatalf("judge states = %d, want 2", len(states))
	}
	if !strings.Contains(states[0], "+uno") || !strings.Contains(states[0], "-one") {
		t.Fatalf("first state = %s, want the one-to-uno diff", states[0])
	}
	if !strings.Contains(states[1], "+una") || !strings.Contains(states[1], "-uno") {
		t.Fatalf("second state = %s, want the uno-to-una diff", states[1])
	}
}

func TestEditReviewMissingAnswerIsError(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{scores: map[string]float64{"sound": 0.2, "placeholder": 0.95}, omit: "lost_code"}
	h := judgedHarness(t, judge, nil)
	h.write("a.txt", "one\ntwo\n")
	thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
	judged := judgedOf(h.events(thread.ID()))
	if len(judged) != 1 || judged[0].Verdict != reviewError || judged[0].Error != "part 1 of 1: answer lost_code is missing or out of range" {
		t.Fatalf("judged = %+v, want the missing answer error", judged)
	}
	if judged[0].FailedParts != 1 || judged[0].Scores != nil {
		t.Fatalf("failed parts = %d scores = %v, want 1 and none", judged[0].FailedParts, judged[0].Scores)
	}
}

func TestEditReviewQuestionsFollowFileKind(t *testing.T) {
	t.Parallel()
	text := reviewQuestionSet([]partFile{{file: &fileDiff{path: "README.md", kind: "text", added: 1}}}, "")
	if got, want := slices.Sorted(maps.Keys(text)), []string{"lost_code", "placeholder", "sound"}; !slices.Equal(got, want) {
		t.Fatalf("text questions = %v, want %v", got, want)
	}
	style := reviewQuestionSet([]partFile{{file: &fileDiff{path: "style.css", kind: "interface", added: 1}}}, "")
	want := []string{"broken_syntax", "layout_motion", "lost_code", "narration", "needless_layer", "off_token", "placeholder", "sound", "unclear_contract", "unsafe_input", "wasted_work", "wrong_logic"}
	if got := slices.Sorted(maps.Keys(style)); !slices.Equal(got, want) {
		t.Fatalf("style questions = %v, want %v", got, want)
	}
}

func TestEditReviewUnguardedStateNeedsConcurrencyLine(t *testing.T) {
	t.Parallel()
	if !hasConcurrentLine([]hunk{{lines: []string{"+\tgo worker()\n"}}}) {
		t.Fatal("go worker() is a concurrency line")
	}
	if hasConcurrentLine([]hunk{{lines: []string{"+\tx := 1\n"}}}) {
		t.Fatal("x := 1 is not a concurrency line")
	}
	asked := reviewQuestionSet([]partFile{{file: &fileDiff{kind: "code", added: 1, concurrent: true}}}, "")
	if _, ok := asked["unguarded_state"]; !ok {
		t.Fatal("unguarded_state is asked for a concurrent code file")
	}
	if _, ok := reviewQuestionSet([]partFile{{file: &fileDiff{kind: "code", added: 1}}}, "")["unguarded_state"]; ok {
		t.Fatal("unguarded_state is asked without a concurrency line")
	}
}

func TestEditReviewUncertainBandDoesNotSpeak(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{scores: map[string]float64{"sound": 0.2, "lost_code": 0.7}}
	h := judgedHarness(t, judge, []string{"lost_code"})
	h.write("a.txt", "one\ntwo\n")
	thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
	judged := judgedOf(h.events(thread.ID()))
	if len(judged) != 1 || judged[0].Verdict != reviewUncertain || judged[0].Flags != nil || judged[0].Spoken != nil || judged[0].Feedback {
		t.Fatalf("judged = %+v, want uncertain with nothing spoken", judged)
	}
}

func TestEditReviewFlagWithSoundAtHalfIsShownNotSpoken(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{scores: map[string]float64{"sound": 0.5, "lost_code": 0.94}}
	h := judgedHarness(t, judge, []string{"lost_code"})
	h.write("a.txt", "one\ntwo\n")
	thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
	judged := judgedOf(h.events(thread.ID()))
	if len(judged) != 1 || judged[0].Verdict != reviewFlagged || !slices.Equal(judged[0].Flags, []string{"lost_code"}) || judged[0].Spoken != nil || judged[0].Feedback {
		t.Fatalf("judged = %+v, want flagged, shown and not spoken", judged)
	}
}

func TestEditReviewFlaggedNonSpeakerIsNotSpoken(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{scores: map[string]float64{"sound": 0.2, "needless_layer": 0.94}}
	h := judgedHarness(t, judge, nil)
	h.write("a.go", "one\ntwo\n")
	thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.go", [2]string{"one", "uno"})}})
	judged := judgedOf(h.events(thread.ID()))
	if len(judged) != 1 || judged[0].Verdict != reviewFlagged || !slices.Equal(judged[0].Flags, []string{"needless_layer"}) || judged[0].Spoken != nil {
		t.Fatalf("judged = %+v, want flagged needless_layer with nothing spoken", judged)
	}
}

func TestEditReviewFeedbackKeyOverrulesSpeakers(t *testing.T) {
	t.Parallel()
	scores := map[string]float64{"sound": 0.2, "lost_code": 0.94, "needless_layer": 0.94}
	withKey := judgedHarness(t, &judgeStub{scores: scores}, []string{"needless_layer"})
	withKey.write("a.go", "one\ntwo\n")
	thread := runRounds(t, withKey, []call{{id: "e1", name: "patch", args: patchArgs("a.go", [2]string{"one", "uno"})}})
	judged := judgedOf(withKey.events(thread.ID()))
	if len(judged) != 1 || !slices.Equal(judged[0].Flags, []string{"lost_code", "needless_layer"}) || !slices.Equal(judged[0].Spoken, []string{"needless_layer"}) {
		t.Fatalf("judged = %+v, want flags lost_code and needless_layer with needless_layer spoken", judged)
	}
	withoutKey := judgedHarness(t, &judgeStub{scores: scores}, nil)
	withoutKey.write("a.go", "one\ntwo\n")
	thread = runRounds(t, withoutKey, []call{{id: "e1", name: "patch", args: patchArgs("a.go", [2]string{"one", "uno"})}})
	if judged := judgedOf(withoutKey.events(thread.ID())); len(judged) != 1 || judged[0].Spoken != nil {
		t.Fatalf("built-in speakers spoke %+v, want none before the measurement", judged)
	}
}

func TestReviewVerdictIgnoresSoundForFlagsAndBand(t *testing.T) {
	t.Parallel()
	if verdict, _ := reviewVerdict(map[string]float64{"sound": 0.95, "lost_code": 0.2}); verdict != reviewClean {
		t.Fatalf("verdict = %s, want clean", verdict)
	}
	if verdict, _ := reviewVerdict(map[string]float64{"sound": 0.7, "lost_code": 0.2}); verdict != reviewClean {
		t.Fatalf("verdict = %s, want clean", verdict)
	}
}

func TestNewHostRejectsUnknownFeedbackID(t *testing.T) {
	t.Parallel()
	for _, id := range []string{"bogus", soundID} {
		h := newHarness(t)
		h.cfg.EditFeedback = []string{id}
		_, err := NewHost(h.cfg)
		if want := "edit_review.feedback: unknown question " + id; err == nil || err.Error() != want {
			t.Fatalf("NewHost error = %v, want %q", err, want)
		}
	}
}

func TestPackPartsKeepsHunkOrder(t *testing.T) {
	t.Parallel()
	a := fileDiff{path: "a.go", change: "update", kind: "code", added: 3, header: "--- a/a.go\n+++ b/a.go\n"}
	for i := range 3 {
		a.hunks = append(a.hunks, hunk{oldStart: i*100 + 1, newStart: i*100 + 1, lines: []string{"+" + strings.Repeat("x", 9990) + "\n"}})
	}
	b := fileDiff{path: "b.go", change: "update", kind: "code", added: 1, header: "--- a/b.go\n+++ b/b.go\n", hunks: []hunk{{oldStart: 1, newStart: 1, lines: []string{"+" + strings.Repeat("y", 9990) + "\n"}}}}
	inputs, omitted, err := packParts("t", "apply_patch", []fileDiff{a, b})
	if err != nil {
		t.Fatal(err)
	}
	if omitted != 0 || len(inputs) != 2 {
		t.Fatalf("parts = %d omitted = %d, want 2 parts and no omission", len(inputs), omitted)
	}
	first, second := partDiffs(t, inputs[0]), partDiffs(t, inputs[1])
	if got := strings.Count(first["a.go"], "@@ "); got != 2 || first["b.go"] != "" {
		t.Fatalf("part 1 holds %d hunks of a.go and b.go %q, want 2 and none", got, first["b.go"])
	}
	if got := strings.Count(second["a.go"], "@@ "); got != 1 || strings.Count(second["b.go"], "@@ ") != 1 {
		t.Fatalf("part 2 holds %d hunks of a.go and %d of b.go, want 1 and 1", got, strings.Count(second["b.go"], "@@ "))
	}
}

func partDiffs(t *testing.T, input judgeInput) map[string]string {
	t.Helper()
	var state struct {
		Files []struct {
			Path string `json:"path"`
			Diff string `json:"diff"`
		} `json:"files"`
	}
	if err := json.Unmarshal(input.state, &state); err != nil {
		t.Fatal(err)
	}
	out := map[string]string{}
	for _, f := range state.Files {
		out[f.Path] = f.Diff
	}
	return out
}

func randomBody(rng *rand.Rand, alphabet []string) string {
	var body strings.Builder
	for range 1 + rng.IntN(5000) {
		body.WriteString(alphabet[rng.IntN(len(alphabet))])
	}
	return body.String()
}

func randomFileDiffs(rng *rand.Rand, alphabet []string) []fileDiff {
	var files []fileDiff
	total, budget := 0, 60000
	for f := range 1 + rng.IntN(4) {
		fd := fileDiff{path: fmt.Sprintf("f%d.go", f), change: "update", kind: "code", header: "--- a/x\n+++ b/x\n"}
		for range 1 + rng.IntN(5) {
			body := randomBody(rng, alphabet)
			if total+len(body) > budget {
				break
			}
			total += len(body)
			fd.hunks = append(fd.hunks, hunk{oldStart: 1, newStart: 1, lines: []string{"+" + body + "\n"}})
			fd.added++
		}
		if len(fd.hunks) > 0 {
			files = append(files, fd)
		}
	}
	return files
}

func partHunksWithinLimit(t *testing.T, input judgeInput) int {
	t.Helper()
	size, err := decide.RequestSize(input.state, input.questions)
	if err != nil {
		t.Fatal(err)
	}
	if size > reviewRequestLimit {
		t.Fatalf("request size = %d, want at most %d", size, reviewRequestLimit)
	}
	hunks := 0
	for _, diff := range partDiffs(t, input) {
		hunks += strings.Count(diff, "@@ ")
	}
	return hunks
}

func checkPackedFiles(t *testing.T, files []fileDiff) {
	t.Helper()
	inputs, omitted, err := packParts("t", "apply_patch", files)
	if err != nil {
		t.Fatal(err)
	}
	if omitted != 0 || len(inputs) > reviewMaxParts {
		t.Fatalf("parts = %d omitted = %d, want at most 8 parts and none omitted", len(inputs), omitted)
	}
	sent := 0
	for _, input := range inputs {
		sent += partHunksWithinLimit(t, input)
	}
	want := 0
	for _, f := range files {
		want += len(f.hunks)
	}
	if sent != want {
		t.Fatalf("hunks sent = %d, want %d", sent, want)
	}
}

func TestPackPartsStaysUnderRequestLimit(t *testing.T) {
	t.Parallel()
	rng := rand.New(rand.NewPCG(1, 2))
	alphabet := []string{"a", "b", `"`, `\`, "\t", "é", " "}
	for range 200 {
		if files := randomFileDiffs(rng, alphabet); len(files) > 0 {
			checkPackedFiles(t, files)
		}
	}
}

func TestSplitHunkCutsAtBlankLine(t *testing.T) {
	t.Parallel()
	h := hunk{oldStart: 1, newStart: 1, lines: []string{" a\n", "+\n", "+func f() {\n", "+\tx := 1\n", "+}\n", "+\n", "+\treturn nil\n"}}
	pieces, complete := splitHunk(h, func(p hunk) bool { return len(p.text()) <= 50 })
	if !complete {
		t.Fatal("split is incomplete")
	}
	want := []string{"@@ -1 +1,2 @@\n a\n+\n", "@@ -2 +3,4 @@\n+func f() {\n+\tx := 1\n+}\n+\n", "@@ -2 +7 @@\n+\treturn nil\n"}
	if len(pieces) != len(want) {
		t.Fatalf("pieces = %d, want %d", len(pieces), len(want))
	}
	for i := range want {
		if got := pieces[i].text(); got != want[i] {
			t.Fatalf("piece %d = %q, want %q", i, got, want[i])
		}
	}
}

func TestSplitHunkRecomputesHeader(t *testing.T) {
	t.Parallel()
	h := hunk{oldStart: 1, newStart: 1, lines: []string{" a\n", "-b\n", "+B\n", " c\n", "+x\n", "+y\n", " d\n"}}
	pieces, complete := splitHunk(h, func(p hunk) bool { return len(p.text()) <= 28 })
	if !complete || len(pieces) != 2 {
		t.Fatalf("pieces = %d complete = %v, want 2 and true", len(pieces), complete)
	}
	if got, want := pieces[0].text(), "@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"; got != want {
		t.Fatalf("piece 1 = %q, want %q", got, want)
	}
	if got, want := pieces[1].text(), "@@ -4 +4,3 @@\n+x\n+y\n d\n"; got != want {
		t.Fatalf("piece 2 = %q, want %q", got, want)
	}
}

func TestReviewTakesWorstPart(t *testing.T) {
	t.Parallel()
	parts := []judgment{
		{by: "judge:x", verdict: reviewClean, scores: map[string]float64{"sound": 0.4, "lost_code": 0.02, "wrong_logic": 0.02}},
		{by: "judge:x", verdict: reviewUncertain, scores: map[string]float64{"sound": 0.2, "wrong_logic": 0.7}},
		{by: "judge:x", verdict: reviewFlagged, scores: map[string]float64{"sound": 0.2, "lost_code": 0.94}, flags: []string{"lost_code"}, spoken: []string{"lost_code"}},
	}
	got := combineParts(parts)
	if got.verdict != reviewFlagged || got.scores["sound"] != 0.2 || got.scores["lost_code"] != 0.94 || got.scores["wrong_logic"] != 0.7 || !slices.Equal(got.flags, []string{"lost_code"}) || !slices.Equal(got.spoken, []string{"lost_code"}) {
		t.Fatalf("combined = %+v, want flagged with the highest scores and lost_code spoken", got)
	}
}

func TestReviewCleanAndFailedPartVerdicts(t *testing.T) {
	t.Parallel()
	clean := judgment{by: "judge:x", verdict: reviewClean, scores: map[string]float64{"sound": 0.4, "lost_code": 0.02, "wrong_logic": 0.02}}
	uncertain := judgment{by: "judge:x", verdict: reviewUncertain, scores: map[string]float64{"sound": 0.2, "wrong_logic": 0.7}}
	if got := combineParts([]judgment{clean}); got.verdict != reviewClean {
		t.Fatalf("single clean part verdict = %s", got.verdict)
	}
	if got := combineParts([]judgment{clean, uncertain}); got.verdict != reviewUncertain {
		t.Fatalf("clean and uncertain verdict = %s, want uncertain", got.verdict)
	}
	if got := combineParts([]judgment{clean, {by: "judge:x", err: errors.New("boom")}}); got.verdict != reviewUncertain || got.failed != 1 {
		t.Fatalf("clean and failed verdict = %s failed = %d, want uncertain and 1", got.verdict, got.failed)
	}
}

func blockEdits(n int) (string, [][2]string) {
	var file strings.Builder
	var pairs [][2]string
	for i := range n {
		fmt.Fprintf(&file, "ln-%02d\n", i)
		for j := range 7 {
			fmt.Fprintf(&file, "gap %02d-%d\n", i, j)
		}
	}
	for i := range n {
		marker := ""
		if i == 2 {
			marker = "THIRDBLOCK "
		}
		pairs = append(pairs, [2]string{fmt.Sprintf("ln-%02d", i), fmt.Sprintf("ln-%02d %s%s", i, marker, strings.Repeat("x", 9990))})
	}
	return file.String(), pairs
}

func TestReviewPartErrorKeepsAnsweredPart(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{scores: map[string]float64{"sound": 0.2, "lost_code": 0.02}, failWhen: "THIRDBLOCK"}
	h := judgedHarness(t, judge, nil)
	file, pairs := blockEdits(3)
	h.write("a.go", file)
	thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.go", pairs...)}})
	judged := judgedOf(h.events(thread.ID()))
	if len(judged) != 1 {
		t.Fatalf("judged = %d, want 1", len(judged))
	}
	got := judged[0]
	if got.Verdict != reviewUncertain || got.Scores["sound"] != 0.2 || got.Scores["lost_code"] != 0.02 || got.Flags != nil || got.Parts != 2 || got.FailedParts != 1 || !got.Truncated {
		t.Fatalf("judged = %+v, want uncertain with the answered part kept, two parts, one failed, truncated", got)
	}
	requireCompletedOK(t, h.events(thread.ID()), "e1")
}

func TestReviewStopsAtMaxParts(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		judge := &judgeStub{}
		h := judgedHarness(t, judge, nil)
		file, pairs := blockEdits(20)
		h.write("a.go", file)
		thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.go", pairs...)}})
		judged := judgedOf(h.events(thread.ID()))
		edits := editStatesOf(judge)
		if len(edits) != 8 || len(judged) != 1 || judged[0].Parts != 8 || !judged[0].Truncated {
			t.Fatalf("judge calls = %d judged = %+v, want 8 requests and a truncated unit of 8 parts", len(edits), judged)
		}
		for _, state := range edits {
			if !strings.Contains(state, `"omitted_hunks":4`) || !strings.Contains(state, `"of":8`) {
				t.Fatalf("state %s lacks omitted_hunks 4 and part of 8", state)
			}
		}
	})
}

func TestEditReviewPartsStayWithinWorkerLimit(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		judge := &judgeStub{hold: 10 * time.Millisecond}
		h := judgedHarness(t, judge, nil)
		file, pairs := blockEdits(20)
		h.write("a.go", file)
		thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.go", pairs...)}})
		judge.mu.Lock()
		peak := judge.peak
		judge.mu.Unlock()
		if edits := len(editStatesOf(judge)); edits != 8 || peak < 2 || peak > 4 {
			t.Fatalf("judge requests = %d, peak in flight = %d, want 8 requests with a peak above 1 and at most 4", edits, peak)
		}
		for _, judged := range judgedOf(h.events(thread.ID())) {
			if !slices.Contains([]string{reviewClean, reviewUncertain, reviewFlagged, reviewError}, judged.Verdict) {
				t.Fatalf("verdict %q is not clean, uncertain, flagged or error", judged.Verdict)
			}
		}
	})
}

func TestEditReviewTruncatedUnitVerdict(t *testing.T) {
	t.Parallel()
	rows := []struct {
		name   string
		scores map[string]float64
		want   string
		flags  []string
	}{
		{name: "answered flag stays flagged", scores: map[string]float64{"lost_code": 0.94}, want: reviewFlagged, flags: []string{"lost_code"}},
		{name: "answered clean becomes uncertain", scores: map[string]float64{"lost_code": 0.02}, want: reviewUncertain},
	}
	for _, row := range rows {
		t.Run(row.name, func(t *testing.T) {
			judge := &judgeStub{scores: row.scores}
			h := judgedHarness(t, judge, []string{"lost_code"})
			file, pairs := blockEdits(20)
			h.write("a.go", file)
			thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.go", pairs...)}})
			judged := judgedOf(h.events(thread.ID()))
			if len(judged) != 1 || !judged[0].Truncated || judged[0].Verdict != row.want || !slices.Equal(judged[0].Flags, row.flags) || judged[0].Feedback != (row.want == reviewFlagged) {
				t.Fatalf("judged = %+v, want a truncated unit with verdict %s, flags %v and feedback only when flagged", judged, row.want, row.flags)
			}
		})
	}
}

func journaledCompleted(th *Thread, callID string) bool {
	if th == nil {
		return false
	}
	journal, err := th.view()
	return err == nil && completedIndex(journal.records, callID) >= 0
}

func TestEditReviewBlockedMaskerJournalsBeforeReturn(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		judge := &judgeStub{}
		h := judgedHarness(t, judge, nil)
		var thread atomic.Pointer[Thread]
		var journaled atomic.Bool
		release := make(chan struct{})
		blockOnce := sync.OnceFunc(func() {
			journaled.Store(journaledCompleted(thread.Load(), "e1"))
			select {
			case <-release:
			case <-time.After(5 * time.Second):
			}
		})
		h.host.reviewMask = func(text string) (string, error) {
			blockOnce()
			return text, nil
		}
		h.write("a.txt", "one\n")
		h.model.push(calls(call{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}), text("done"))
		started := h.start()
		thread.Store(started)
		h.submit(started, "fix it")
		deadline := time.Now().Add(5 * time.Second)
		for !journaledCompleted(started, "e1") && time.Now().Before(deadline) {
			time.Sleep(time.Millisecond)
		}
		close(release)
		h.waitIdle(started)
		if !journaled.Load() {
			t.Fatal("the review masker ran before tool.completed was journaled")
		}
	})
}

func TestJudgedPathsWithheldWhenMaskingFails(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.host.reviewMask = func(string) (string, error) { return "", errors.New("masker down") }
	h.write("a.txt", "one\n")
	thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
	judged := judgedOf(h.events(thread.ID()))
	if len(judged) != 1 || !slices.Equal(judged[0].Paths, []string{reviewPathsWithheld}) || judged[0].Verdict != reviewError {
		t.Fatalf("judged = %+v, want the withheld path and an error verdict", judged)
	}
	raw, err := event.Encode(judged[0])
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(raw), "a.txt") {
		t.Fatalf("tool.judged holds the raw path: %s", raw)
	}
}

func TestLoadReviewFileClassifiesMaskedBytes(t *testing.T) {
	t.Parallel()
	rows := []struct {
		name string
		raw  string
		want string
	}{
		{name: "masked text grows past the limit", raw: "tok" + strings.Repeat("a", textdiff.Limit-3), want: "large"},
		{name: "masked text shrinks under the limit", raw: "SECRET" + strings.Repeat("a", textdiff.Limit-5), want: ""},
	}
	for _, row := range rows {
		t.Run(row.name, func(t *testing.T) {
			t.Parallel()
			h := newHarness(t, withApproval(config.ApprovalYolo))
			h.host.reviewMask = func(text string) (string, error) {
				text = strings.ReplaceAll(text, "tok", "tok"+strings.Repeat("y", 10))
				return strings.ReplaceAll(text, "SECRET", "x"), nil
			}
			h.write("big.txt", row.raw)
			sum := sha256.Sum256([]byte(row.raw))
			thread := h.start()
			w := &worker{t: thread}
			change := event.FileChange{Path: "big.txt", PreSHA: "pre", PostSHA: hex.EncodeToString(sum[:])}
			file, err := w.loadReviewFile(t.Context(), change, "big.txt")
			if err != nil {
				t.Fatal(err)
			}
			if file.skip != row.want {
				t.Fatalf("skip = %q, want %q", file.skip, row.want)
			}
		})
	}
}

func TestRequireMaskedRejectsChangedState(t *testing.T) {
	t.Parallel()
	mask := func(s string) (string, error) { return strings.ReplaceAll(s, "SECRET", "x"), nil }
	if err := requireMasked(mask, jsontext.Value(`{"a":"SECRET"}`)); !errors.Is(err, errReviewSecret) {
		t.Fatalf("error = %v, want errReviewSecret", err)
	}
	if err := requireMasked(mask, jsontext.Value(`{"a":"x"}`)); err != nil {
		t.Fatalf("error = %v, want nil", err)
	}
}

func seededEdit(h *harness) []event.Event {
	args := patchArgs("a.txt", [2]string{"one", "uno"})
	return append(openTurn("fix it"),
		assistantCall("e1", "patch", args),
		event.ToolStarted{CallID: "e1", TurnID: "t", Tool: "patch", Class: "edit", Args: jsontext.Value(args)},
	)
}

func TestRecoveryNeverJudgesCompletedEdit(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.write("a.txt", "uno\n")
	h.seed(append(seededEdit(h), event.ToolCompleted{CallID: "e1", OK: true, Output: "patched", Recovered: "none"})...)
	h.model.push(text("done"))
	events := h.recoverSession()
	if judge.count() != 0 || len(judgedOf(events)) != 0 {
		t.Fatalf("judge calls = %d, want none on recovery", judge.count())
	}
}

func TestEditReviewFactoryHangFailsOpen(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
			h.cfg.Config.EditReview.Enabled = true
			h.cfg.EditJudge = func(ctx context.Context, _ string) (decide.Client, string, error) {
				<-ctx.Done()
				return nil, "", ctx.Err()
			}
		})
		h.host.reviewTimeout = 50 * time.Millisecond
		h.write("a.txt", "one\ntwo\n")
		start := time.Now()
		thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
		if elapsed := time.Since(start); elapsed > 2*time.Second {
			t.Fatalf("review took %s, want under 2s", elapsed)
		}
		events := h.events(thread.ID())
		judged := judgedOf(events)
		if len(judged) != 1 || judged[0].Verdict != reviewError || !strings.Contains(judged[0].Error, "review timed out after 50ms") {
			t.Fatalf("judged = %+v, want one error verdict naming the 50ms timeout", judged)
		}
		requireCompletedOK(t, events, "e1")
	})
}

func TestRecoveryDoesNotJudgeReapprovedEdit(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.write("a.txt", "one\ntwo\n")
	args := patchArgs("a.txt", [2]string{"one", "uno"})
	h.seed(append(openTurn("fix it"),
		assistantCall("e1", "patch", args),
		event.ApprovalRequested{RequestID: "req-1", CallID: "e1", Summary: "patch a.txt", Class: "edit"},
	)...)
	h.model.push(text("done"))
	events := h.recoverSession()
	if judge.count() != 0 || len(judgedOf(events)) != 0 {
		t.Fatalf("judge calls = %d, tool.judged = %d, want none on recovery", judge.count(), len(judgedOf(events)))
	}
	if got := h.read("a.txt"); got != "uno\ntwo\n" {
		t.Fatalf("a.txt = %q, want the re-approved edit applied", got)
	}
}

func TestReviewSummary(t *testing.T) {
	long := strings.Repeat("e", 200)
	cases := []struct {
		name      string
		verdict   string
		scores    map[string]float64
		flags     []string
		spoken    []string
		errText   string
		truncated bool
		want      string
	}{
		{name: "clean", verdict: "clean", want: "No problems found"},
		{name: "flagged and spoken", verdict: "flagged", flags: []string{"broken_syntax", "lost_code"}, spoken: []string{"broken_syntax"}, want: "Flagged: broken syntax, removed code"},
		{name: "flagged not sent", verdict: "flagged", flags: []string{"mystery"}, want: "Flagged: mystery (not sent to the model)"},
		{name: "uncertain labels in table order", verdict: "uncertain", scores: map[string]float64{"lost_code": 0.62, "placeholder": 0.71, "sound": 0.6, "broken_syntax": 0.1}, want: "Uncertain: removed code, placeholder"},
		{name: "error cut to 120 bytes", verdict: "error", errText: long, want: "Failed: " + long[:120]},
		{name: "clean with unsent hunks", verdict: "clean", truncated: true, want: "No problems found"},
		{name: "flagged spoken and truncated", verdict: "flagged", flags: []string{"broken_syntax", "lost_code"}, spoken: []string{"broken_syntax"}, truncated: true, want: "Flagged: broken syntax, removed code; part of the diff was not judged"},
		{name: "flagged not sent and truncated", verdict: "flagged", flags: []string{"mystery"}, truncated: true, want: "Flagged: mystery (not sent to the model); part of the diff was not judged"},
		{name: "uncertain without label and truncated", verdict: "uncertain", scores: map[string]float64{"broken_syntax": 0.1, "sound": 0.2}, truncated: true, want: "Uncertain: part of the diff was not judged"},
		{name: "uncertain without label", verdict: "uncertain", scores: map[string]float64{"broken_syntax": 0.1, "sound": 0.2}, want: "Uncertain: no label applies"},
		{name: "uncertain labels and truncated", verdict: "uncertain", scores: map[string]float64{"lost_code": 0.62, "placeholder": 0.71, "sound": 0.6, "broken_syntax": 0.1}, truncated: true, want: "Uncertain: removed code, placeholder; part of the diff was not judged"},
		{name: "error with unsent hunks", verdict: "error", errText: long, truncated: true, want: "Failed: " + long[:120]},
		{name: "uncertain at the lower bound", verdict: "uncertain", scores: map[string]float64{"lost_code": 0.5}, want: "Uncertain: removed code"},
		{name: "uncertain stops below the flag threshold", verdict: "uncertain", scores: map[string]float64{"lost_code": 0.9}, want: "Uncertain: no label applies"},
		{name: "error cut at a character boundary", verdict: "error", errText: "a" + strings.Repeat("가", 50), want: "Failed: a" + strings.Repeat("가", 39)},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := ReviewSummary(ReviewOutcome{Verdict: tc.verdict, Scores: tc.scores, Flags: tc.flags, Spoken: tc.spoken, Error: tc.errText, Truncated: tc.truncated})
			if got != tc.want {
				t.Fatalf("ReviewSummary = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestEditReviewSavedToggleControlsOpenThread(t *testing.T) {
	judge := &judgeStub{}
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.EditJudge = judgeFor(judge)
		h.cfg.Config.EditReview.Enabled = false
	})
	h.write("a.txt", "zero\n")
	thread := h.start()
	steps := []struct {
		enabled bool
		before  string
		after   string
		judged  int
	}{
		{false, "zero", "one", 0},
		{true, "one", "two", 1},
		{false, "two", "three", 1},
		{true, "three", "four", 2},
	}
	for i, step := range steps {
		if err := h.host.WriteSettings(config.SettingEdit{Key: "edit_review.enabled", Value: step.enabled}); err != nil {
			t.Fatal(err)
		}
		settings, err := os.ReadFile(filepath.Join(h.home, "settings.toml"))
		if err != nil || !strings.Contains(string(settings), fmt.Sprintf("enabled = %t", step.enabled)) {
			t.Fatalf("saved settings = %q, %v", settings, err)
		}
		id := fmt.Sprintf("edit-%d", i)
		h.model.push(calls(call{id: id, name: "patch", args: patchArgs("a.txt", [2]string{step.before, step.after})}), text("done"))
		h.submit(thread, "change the value")
		h.waitIdle(thread)
		events := h.events(thread.ID())
		requireCompletedOK(t, events, id)
		content, err := os.ReadFile(filepath.Join(h.cwd, "a.txt"))
		if err != nil || string(content) != step.after+"\n" {
			t.Fatalf("edit %s wrote %q, %v", id, content, err)
		}
		requireReviewCounts(t, events, judge, step.judged, step.judged)
	}
}

func TestAdmittedEditReviewDrainsAfterSavedOff(t *testing.T) {
	entered := make(chan struct{})
	release := make(chan struct{})
	unblock := sync.OnceFunc(func() { close(release) })
	defer unblock()
	admitted := sync.OnceFunc(func() { close(entered) })
	judge := &judgeStub{}
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.Config.EditReview.Enabled = true
		h.cfg.EditJudge = func(ctx context.Context, id string) (decide.Client, string, error) {
			admitted()
			select {
			case <-release:
				return judgeFor(judge)(ctx, id)
			case <-ctx.Done():
				return nil, "", ctx.Err()
			}
		}
	})
	h.write("a.txt", "one\n")
	h.model.push(calls(call{id: "edit", name: "patch", args: patchArgs("a.txt", [2]string{"one", "two"})}), text("done"))
	thread := h.start()
	h.submit(thread, "change the value")
	select {
	case <-entered:
	case <-t.Context().Done():
		t.Fatal(t.Context().Err())
	}
	if err := h.host.WriteSettings(config.SettingEdit{Key: "edit_review.enabled", Value: false}); err != nil {
		t.Fatal(err)
	}
	unblock()
	h.waitIdle(thread)
	events := h.events(thread.ID())
	requireCompletedOK(t, events, "edit")
	content, err := os.ReadFile(filepath.Join(h.cwd, "a.txt"))
	if err != nil || string(content) != "two\n" {
		t.Fatalf("edit wrote %q, %v", content, err)
	}
	requireReviewCounts(t, events, judge, 1, 0)
}

func requireReviewCounts(t *testing.T, events []event.Seqd, judge *judgeStub, wantEdits, wantTurns int) {
	t.Helper()
	if edits, turns := len(judgedOf(events)), len(turnJudgedOf(events)); edits != wantEdits || turns != wantTurns {
		t.Fatalf("edit and turn verdicts = %d, %d, want %d, %d", edits, turns, wantEdits, wantTurns)
	}
	if edits, turns := len(editStatesOf(judge)), len(turnStatesOf(judge)); edits != wantEdits || turns != wantTurns {
		t.Fatalf("edit and turn requests = %d, %d, want %d, %d", edits, turns, wantEdits, wantTurns)
	}
}
