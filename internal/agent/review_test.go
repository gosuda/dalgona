package agent

import (
	"cmp"
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
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/decide"
	"github.com/gosuda/dalgona/internal/dal/textdiff"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

type judgeStub struct {
	mu       sync.Mutex
	scores   map[string]float64
	omit     string
	failWhen string
	err      error
	errs     []error
	model    string
	fallback string
	altWhen  string
	altModel string
	started  chan struct{}
	once     sync.Once
	block    bool
	hold     time.Duration
	flight   int
	peak     int
	states   []string
	asked    [][]string
}

func (j *judgeStub) Ask(ctx context.Context, state jsontext.Value, questions map[string]decide.Question) (decide.Result, error) {
	ids := slices.Sorted(maps.Keys(questions))
	index := j.recordAsk(state, ids)
	defer func() {
		j.mu.Lock()
		j.flight--
		j.mu.Unlock()
	}()
	if j.started != nil {
		j.once.Do(func() { close(j.started) })
	}
	if j.hold > 0 {
		time.Sleep(j.hold)
	}
	if j.block {
		<-ctx.Done()
		return decide.Result{}, ctx.Err()
	}
	if err := j.failure(index, state); err != nil {
		return decide.Result{}, err
	}
	model, fallback := cmp.Or(j.model, "stub-judge"), j.fallback
	if j.altWhen != "" && strings.Contains(string(state), j.altWhen) {
		model, fallback = j.altModel, ""
	}
	return decide.Result{Model: model, Fallback: fallback, Answers: j.answers(ids)}, nil
}

func (j *judgeStub) recordAsk(state jsontext.Value, ids []string) int {
	j.mu.Lock()
	defer j.mu.Unlock()
	j.states = append(j.states, string(state))
	j.asked = append(j.asked, ids)
	index := len(j.states) - 1
	j.flight++
	j.peak = max(j.peak, j.flight)
	return index
}

func (j *judgeStub) failure(index int, state jsontext.Value) error {
	if j.err != nil {
		return j.err
	}
	if index < len(j.errs) && j.errs[index] != nil {
		return j.errs[index]
	}
	if j.failWhen != "" && strings.Contains(string(state), j.failWhen) {
		return errors.New("boom")
	}
	return nil
}

func (j *judgeStub) answers(ids []string) map[string]decide.Answer {
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
	return answers
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

type failOpenRow struct {
	name      string
	factory   ClientFactory
	timeout   time.Duration
	fillSlots bool
	want      string
}

func TestEditReviewFailsOpen(t *testing.T) {
	t.Parallel()
	rows := []failOpenRow{
		{name: "factory error", factory: func(context.Context, string) (decide.Client, string, error) {
			return nil, "", errors.New("no credentials")
		}, want: "no credentials"},
		{name: "ask error", factory: judgeFor(&judgeStub{err: errors.New("boom")}), want: "part 1 of 1: boom"},
		{name: "slot wait times out", factory: judgeFor(&judgeStub{}), timeout: 50 * time.Millisecond, fillSlots: true, want: "part 1 of 1: review waited 50ms for a judge slot"},
	}
	for _, row := range rows {
		runFailOpenRow(t, row)
	}
}

func runFailOpenRow(t *testing.T, row failOpenRow) {
	t.Helper()
	t.Run(row.name, func(t *testing.T) {
		t.Parallel()
		synctest.Test(t, func(t *testing.T) {
			h, release := failOpenHarness(t, row)
			defer release()
			h.write("a.txt", "one\ntwo\n")
			thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
			requireFailOpenVerdict(t, h, thread, row.want)
		})
	})
}

func failOpenHarness(t *testing.T, row failOpenRow) (*harness, func()) {
	t.Helper()
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.EditJudge = row.factory
		h.cfg.Config.EditReview.Enabled = true
	})
	if row.timeout > 0 {
		h.host.reviewTimeout = row.timeout
	}
	release := func() {}
	if row.fillSlots {
		release = occupyReviewSlots(h)
	}
	return h, release
}

func occupyReviewSlots(h *harness) (release func()) {
	for range cap(h.host.reviewSlots) {
		h.host.reviewSlots <- struct{}{}
	}
	return func() {
		for range cap(h.host.reviewSlots) {
			<-h.host.reviewSlots
		}
	}
}

func requireFailOpenVerdict(t *testing.T, h *harness, thread *Thread, want string) {
	t.Helper()
	events := h.events(thread.ID())
	judged := judgedOf(events)
	if len(judged) != 1 || judged[0].Verdict != reviewError || !strings.Contains(judged[0].Error, want) {
		t.Fatalf("judged = %+v, want error containing %q", judged, want)
	}
	if done := events[completedIndex(events, "e1")].Event.(event.ToolCompleted); !done.OK {
		t.Fatalf("tool.completed.OK = false, want the edit to stand")
	}
}

func TestEditReviewRecordsFallback(t *testing.T) {
	t.Parallel()
	t.Run("a fallback answer names the answering model", func(t *testing.T) {
		t.Parallel()
		judge := &judgeStub{model: "smol-model", fallback: "primary-model: no credentials", scores: map[string]float64{"sound": 0.2, "lost_code": 0.94}}
		h := judgedHarness(t, judge, []string{"lost_code"})
		h.write("a.txt", "one\ntwo\n")
		thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
		requireJudgeBy(t, judgedOf(h.events(thread.ID())), "judge:smol-model")
	})
	t.Run("a turn fallback answer names the answering model", func(t *testing.T) {
		t.Parallel()
		judge := &judgeStub{model: "smol-model", fallback: "primary-model: no credentials", scores: map[string]float64{"sound": 0.2, "incomplete": 0.94}}
		h := judgedHarness(t, judge, []string{"incomplete"})
		h.write("a.txt", "one\n")
		thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
		requireTurnJudgeBy(t, turnJudgedOf(h.events(thread.ID())), "judge:smol-model")
	})
	t.Run("mixed models keep ordered part attribution", func(t *testing.T) {
		t.Parallel()
		combined := combineParts([]judgment{
			{by: "judge:smol-model", fallback: "primary-a: quota", verdict: reviewFlagged, flags: []string{"lost_code"}, spoken: []string{"lost_code"}},
			{by: "judge:primary-b", verdict: reviewClean},
		})
		requireCombinedIdentity(t, combined, "judge:mixed", "primary-a: quota")
		want := []JudgeAttribution{
			{Part: 0, By: "judge:smol-model", Fallback: "primary-a: quota"},
			{Part: 1, By: "judge:primary-b"},
		}
		if !slices.Equal(combined.judges, want) {
			t.Fatalf("judges = %+v, want %+v", combined.judges, want)
		}
	})
	t.Run("one answering model keeps its name", func(t *testing.T) {
		t.Parallel()
		combined := combineParts([]judgment{
			{by: "judge:smol-model", fallback: "primary-a: quota", verdict: reviewClean},
			{by: "judge:smol-model", fallback: "primary-a: quota", verdict: reviewFlagged, flags: []string{"lost_code"}},
		})
		requireCombinedIdentity(t, combined, "judge:smol-model", "primary-a: quota")
		if len(combined.judges) != 2 {
			t.Fatalf("judges = %+v, want both parts attributed", combined.judges)
		}
	})
}

func requireJudgeBy(t *testing.T, judged []event.ToolJudged, want string) {
	t.Helper()
	if len(judged) != 1 || judged[0].By != want {
		t.Fatalf("judged = %+v, want by to name the answering model", judged)
	}
}

func requireTurnJudgeBy(t *testing.T, turns []event.TurnJudged, want string) {
	t.Helper()
	if len(turns) != 1 || turns[0].By != want {
		t.Fatalf("turn.judged = %+v, want by to name the answering model", turns)
	}
}

func requireCombinedIdentity(t *testing.T, combined judgment, by, fallback string) {
	t.Helper()
	if combined.by != by || combined.fallback != fallback {
		t.Fatalf("combined = %+v, want %s with fallback %q", combined, by, fallback)
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
	requireReviewCounts(t, h.events(thread.ID()), judge, 0, 0)
	h.model.push(calls(call{id: "e2", name: "patch", args: patchArgs("a.txt", [2]string{"two", "three"})}), text("done"))
	h.submit(thread, "fix it again")
	h.waitIdle(thread)
	requireReviewCounts(t, h.events(thread.ID()), judge, 1, 1)
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
	events := h.events(thread.ID())
	judged := judgedOf(events)
	if len(judged) != 1 || judged[0].Verdict != reviewError || judged[0].Error != "part 1 of 1: answer lost_code is missing or out of range" {
		t.Fatalf("judged = %+v, want the missing answer error", judged)
	}
	if judged[0].FailedParts != 1 || judged[0].Scores != nil {
		t.Fatalf("failed parts = %d scores = %v, want 1 and none", judged[0].FailedParts, judged[0].Scores)
	}
	requireCompletedOK(t, events, "e1")
	if last := lastTurn(events); last.Status != statusCompleted {
		t.Fatalf("turn status = %q, want completed", last.Status)
	}
	if got := h.read("a.txt"); got != "uno\ntwo\n" {
		t.Fatalf("a.txt = %q, want the edit applied", got)
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
	valid := newHarness(t)
	valid.cfg.EditFeedback = []string{"lost_code"}
	if _, err := NewHost(valid.cfg); err != nil {
		t.Fatalf("NewHost error = %v, want a known question id accepted", err)
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

func hunklessFixture(t *testing.T) []fileDiff {
	t.Helper()
	same := []byte("one\ntwo\n")
	files, err := diffFiles([]reviewFile{
		classifyReviewFile("a.go", "update", []byte("one\n"), same),
		classifyReviewFile("b.go", "update", same, same),
	}, reviewContextLines)
	if err != nil {
		t.Fatal(err)
	}
	if len(files) != 2 || len(files[1].hunks) != 0 {
		t.Fatalf("diffFiles gave %d files, want both with the second holding no hunk", len(files))
	}
	return files
}

func TestPackPartsKeepsFileWithoutHunkAsOneUnit(t *testing.T) {
	t.Parallel()
	inputs, omitted, err := packParts("t", "apply_patch", hunklessFixture(t))
	if err != nil {
		t.Fatal(err)
	}
	if omitted != 0 || len(inputs) != 1 {
		t.Fatalf("parts = %d omitted = %d, want 1 part and no omission", len(inputs), omitted)
	}
	requireHunklessEntry(t, partDiffs(t, inputs[0]))
}

func requireHunklessEntry(t *testing.T, diffs map[string]string) {
	t.Helper()
	if empty, ok := diffs["b.go"]; !ok || empty != "" {
		t.Fatalf("b.go entry = %q present %t, want an entry with an empty diff", empty, ok)
	}
	if !strings.Contains(diffs["a.go"], "+two") {
		t.Fatalf("a.go diff = %q, want the added line", diffs["a.go"])
	}
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
	for _, f := range files {
		if joinedHunks(t, f, inputs) != inputHunks(f) {
			t.Fatalf("hunks of %s joined across the parts differ from the input hunks in order", f.path)
		}
	}
}

func joinedHunks(t *testing.T, f fileDiff, inputs []judgeInput) string {
	t.Helper()
	var joined strings.Builder
	for _, input := range inputs {
		joined.WriteString(strings.TrimPrefix(partDiffs(t, input)[f.path], f.header))
	}
	return joined.String()
}

func inputHunks(f fileDiff) string {
	var joined strings.Builder
	for _, h := range f.hunks {
		joined.WriteString(h.text())
	}
	return joined.String()
}

func TestPackPartsOrderCheckSeesReorderedParts(t *testing.T) {
	t.Parallel()
	a := fileDiff{path: "a.go", change: "update", kind: "code", added: 3, header: "--- a/a.go\n+++ b/a.go\n"}
	for i, fill := range []string{"x", "y", "z"} {
		a.hunks = append(a.hunks, hunk{oldStart: i*100 + 1, newStart: i*100 + 1, lines: []string{"+" + strings.Repeat(fill, 9990) + "\n"}})
	}
	inputs, omitted, err := packParts("t", "apply_patch", []fileDiff{a})
	if err != nil {
		t.Fatal(err)
	}
	if omitted != 0 || len(inputs) != 2 {
		t.Fatalf("parts = %d omitted = %d, want 2 parts and no omission", len(inputs), omitted)
	}
	if joinedHunks(t, a, inputs) != inputHunks(a) {
		t.Fatal("packed parts do not join to the input hunks in order")
	}
	if joinedHunks(t, a, []judgeInput{inputs[1], inputs[0]}) == inputHunks(a) {
		t.Fatal("reordered parts still join to the input order, so the check cannot fail")
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
			t.Parallel()
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

func seededEdit() []event.Event {
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
	h.seed(append(seededEdit(), event.ToolCompleted{CallID: "e1", OK: true, Output: "patched", Recovered: "none"})...)
	h.model.push(calls(call{id: "e2", name: "patch", args: patchArgs("a.txt", [2]string{"uno", "dos"})}), text("done"))
	events := h.recoverSession()
	requireReviewCounts(t, events, judge, 1, 0)
}

func TestEditReviewFactoryHangFailsOpen(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		entered := make(chan struct{})
		h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
			h.cfg.Config.EditReview.Enabled = true
			h.cfg.EditJudge = func(ctx context.Context, _ string) (decide.Client, string, error) {
				close(entered)
				<-ctx.Done()
				return nil, "", ctx.Err()
			}
		})
		h.write("a.txt", "one\ntwo\n")
		h.model.push(calls(call{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}), text("done"))
		thread := h.start()
		h.submit(thread, "fix it")
		<-entered
		thread.Interrupt()
		h.waitIdle(thread)
		events := h.events(thread.ID())
		judged := judgedOf(events)
		if len(judged) != 1 || judged[0].Verdict != reviewError || !strings.Contains(judged[0].Error, "review: the turn was interrupted") {
			t.Fatalf("judged = %+v, want one error verdict naming the interrupt", judged)
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
	h.model.push(calls(call{id: "e2", name: "patch", args: patchArgs("a.txt", [2]string{"uno", "dos"})}), text("done"))
	events := h.recoverSession()
	requireReviewCounts(t, events, judge, 1, 0)
	if got := h.read("a.txt"); got != "dos\ntwo\n" {
		t.Fatalf("a.txt = %q, want the re-approved edit and the new edit applied", got)
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
	t.Parallel()
	rows := []struct {
		name    string
		disable bool
		turns   int
	}{
		{name: "saved off after admission", disable: true},
		{name: "toggle left on", turns: 1},
	}
	for _, row := range rows {
		runAdmittedDrainCase(t, row.name, row.disable, row.turns)
	}
}

func runAdmittedDrainCase(t *testing.T, name string, disable bool, turns int) {
	t.Helper()
	t.Run(name, func(t *testing.T) {
		t.Parallel()
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
		waitAdmittedJudge(t, entered)
		if disable {
			if err := h.host.WriteSettings(config.SettingEdit{Key: "edit_review.enabled", Value: false}); err != nil {
				t.Fatal(err)
			}
		}
		unblock()
		h.waitIdle(thread)
		requireAdmittedDrainOutcome(t, h, thread, judge, turns)
	})
}

func waitAdmittedJudge(t *testing.T, entered chan struct{}) {
	t.Helper()
	select {
	case <-entered:
	case <-t.Context().Done():
		t.Fatal(t.Context().Err())
	}
}

func requireAdmittedDrainOutcome(t *testing.T, h *harness, thread *Thread, judge *judgeStub, turns int) {
	t.Helper()
	events := h.events(thread.ID())
	requireCompletedOK(t, events, "edit")
	content, err := os.ReadFile(filepath.Join(h.cwd, "a.txt"))
	if err != nil || string(content) != "two\n" {
		t.Fatalf("edit wrote %q, %v", content, err)
	}
	requireReviewCounts(t, events, judge, 1, turns)
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

func judgedFollowsCompletion(events []event.Seqd, callID string) bool {
	done, judged := -1, -1
	for i, item := range events {
		switch value := item.Event.(type) {
		case event.ToolCompleted:
			if value.CallID == callID {
				done = i
			}
		case event.ToolJudged:
			if value.CallID == callID && judged < 0 {
				judged = i
			}
		}
	}
	return done >= 0 && judged > done
}

func TestEditReviewStopsAfterTwoFailedUnitsInATurn(t *testing.T) {
	t.Parallel()
	rounds := [][]call{
		{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"zero", "one"})}},
		{{id: "e2", name: "patch", args: patchArgs("a.txt", [2]string{"one", "two"})}},
		{{id: "e3", name: "patch", args: patchArgs("a.txt", [2]string{"two", "three"})}},
		{{id: "e4", name: "patch", args: patchArgs("a.txt", [2]string{"three", "four"})}},
	}
	t.Run("a failing judge stops after two units", func(t *testing.T) {
		t.Parallel()
		judge := &judgeStub{err: errors.New("down")}
		h := judgedHarness(t, judge, []string{"lost_code"})
		h.write("a.txt", "zero\n")
		thread := runRounds(t, h, rounds...)
		requireBreakerStoppedReviews(t, h, thread, judge)
	})
	t.Run("an answering judge reviews every unit", func(t *testing.T) {
		t.Parallel()
		judge := &judgeStub{}
		h := judgedHarness(t, judge, []string{"lost_code"})
		h.write("a.txt", "zero\n")
		thread := runRounds(t, h, rounds...)
		requireReviewCounts(t, h.events(thread.ID()), judge, 4, 1)
	})
	t.Run("a clean verdict resets the counter", func(t *testing.T) {
		t.Parallel()
		judge := &judgeStub{errs: []error{errors.New("down"), nil, errors.New("down"), nil}}
		h := judgedHarness(t, judge, []string{"lost_code"})
		h.write("a.txt", "zero\n")
		thread := runRounds(t, h, rounds...)
		requireResetCounterVerdicts(t, h, thread, judge)
	})
}

func requireBreakerStoppedReviews(t *testing.T, h *harness, thread *Thread, judge *judgeStub) {
	t.Helper()
	events := h.events(thread.ID())
	judged := judgedOf(events)
	if len(judged) != 2 || judge.count() != 2 {
		t.Fatalf("verdicts and requests = %d, %d, want 2 and 2", len(judged), judge.count())
	}
	for _, entry := range judged {
		if entry.Verdict != reviewError {
			t.Fatalf("verdict = %q, want error", entry.Verdict)
		}
	}
	if turns := turnJudgedOf(events); len(turns) != 0 {
		t.Fatalf("turn verdicts = %d, want none after the breaker", len(turns))
	}
	for _, id := range []string{"e1", "e2", "e3", "e4"} {
		requireCompletedOK(t, events, id)
	}
}

func requireResetCounterVerdicts(t *testing.T, h *harness, thread *Thread, judge *judgeStub) {
	t.Helper()
	events := h.events(thread.ID())
	judged := judgedOf(events)
	want := []string{reviewError, reviewClean, reviewError, reviewClean}
	if len(judged) != len(want) {
		t.Fatalf("verdicts = %d, want %d", len(judged), len(want))
	}
	for i, verdict := range want {
		if judged[i].Verdict != verdict {
			t.Fatalf("verdict %d = %q, want %q", i, judged[i].Verdict, verdict)
		}
	}
	requireReviewCounts(t, events, judge, 4, 1)
}

type gatedJudge struct {
	gate    chan struct{}
	started chan struct{}
	once    sync.Once
	inner   *judgeStub
}

func (g *gatedJudge) Ask(ctx context.Context, state jsontext.Value, questions map[string]decide.Question) (decide.Result, error) {
	g.once.Do(func() { close(g.started) })
	select {
	case <-g.gate:
	case <-ctx.Done():
		return decide.Result{}, ctx.Err()
	}
	return g.inner.Ask(ctx, state, questions)
}

type nestedCell struct {
	thread *Thread
	calls  []event.ToolCall
}

func (c *nestedCell) Spec() tool.Spec {
	return tool.Spec{Name: "cell", Description: "Run nested tool calls.", Schema: jsontext.Value(`{"type":"object","properties":{},"additionalProperties":false}`)}
}

func (c *nestedCell) Class() tool.Class { return tool.ClassScript }

func (c *nestedCell) Run(ctx context.Context, call tool.Call, _ tool.Env) (tool.Result, error) {
	source, err := c.thread.PutBlob([]byte("cell"))
	if err != nil {
		return tool.Result{}, err
	}
	started := event.ScriptStarted{RunID: call.ID, SourceBlob: source, Entry: "cell", Args: jsontext.Value("null")}
	if err := c.thread.Append(context.Background(), started); err != nil {
		return tool.Result{}, err
	}
	cellCtx, cancel := context.WithCancelCause(ctx)
	errs := make([]error, len(c.calls))
	var group sync.WaitGroup
	for i, nested := range c.calls {
		group.Go(func() { errs[i] = c.thread.RunNestedCall(cellCtx, nested) })
	}

	group.Wait()
	cancel(nil)
	joined := errors.Join(errs...)
	done := event.ScriptCompleted{RunID: call.ID, OK: joined == nil, Value: jsontext.Value("null")}
	if joined != nil {
		done = event.ScriptCompleted{RunID: call.ID, Error: joined.Error()}
	}
	if err := c.thread.Append(context.Background(), done); err != nil {
		return tool.Result{Output: []byte(err.Error())}, err
	}
	if joined != nil {
		return tool.Result{Output: []byte(joined.Error())}, joined
	}
	return tool.Result{Output: []byte("ok"), OK: true}, nil
}

func cellCalls() []event.ToolCall {
	return []event.ToolCall{
		{ID: "run-1/1", Name: "patch", Args: jsontext.Value(patchArgs("a.txt", [2]string{"one", "uno"}))},
		{ID: "run-1/2", Name: "patch", Args: jsontext.Value(patchArgs("b.txt", [2]string{"one", "uno"}))},
		{ID: "run-1/3", Name: "read", Args: jsontext.Value(`{"path":"a.txt"}`)},
	}
}

func TestEditReviewJudgesEditsMadeInsideCell(t *testing.T) {
	t.Parallel()
	nested := cellCalls()
	t.Run("a cell edit is judged after the cell returns", func(t *testing.T) {
		t.Parallel()
		gate := make(chan struct{})
		started := make(chan struct{})
		judge := &judgeStub{}
		gated := &gatedJudge{gate: gate, started: started, inner: judge}
		cell := &nestedCell{calls: nested}
		h := cellHarness(t, cell, true, func(context.Context, string) (decide.Client, string, error) {
			return gated, "stub-judge", nil
		})
		thread := h.start()
		cell.thread = thread
		h.submit(thread, "run the cell")
		<-started
		if n := len(judgedOf(h.events(thread.ID()))); n != 0 {
			t.Fatalf("verdicts before the cell returned = %d, want none", n)
		}
		h.waitEvent(thread.ID(), cellCompleted)
		close(gate)
		h.waitIdle(thread)
		requireCellVerdictsAfterReturn(t, h, thread, judge)
	})
	t.Run("review off journals no verdict", func(t *testing.T) {
		cell := &nestedCell{calls: nested}
		judge := &judgeStub{}
		h := cellHarness(t, cell, false, judgeFor(judge))
		thread := h.start()
		cell.thread = thread
		h.submit(thread, "run the cell")
		h.waitIdle(thread)
		requireNoCellReviewWhenOff(t, h, thread, judge)
	})
	t.Run("a turn interrupt fails the cell review", func(t *testing.T) {
		t.Parallel()
		cell := &nestedCell{calls: nested}
		judge := &judgeStub{block: true, started: make(chan struct{})}
		h := cellHarness(t, cell, true, judgeFor(judge))
		thread := h.start()
		cell.thread = thread
		h.submit(thread, "run the cell")
		<-judge.started
		thread.Interrupt()
		h.waitIdle(thread)
		requireInterruptedCellReview(t, h.events(thread.ID()))
	})
}

func cellHarness(t *testing.T, cell *nestedCell, enabled bool, factory ClientFactory) *harness {
	t.Helper()
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.EditJudge = factory
		if enabled {
			h.cfg.Config.EditReview.Enabled = true
		}
	})
	h.extraTools = append(h.extraTools, cell)
	h.write("a.txt", "one\n")
	h.write("b.txt", "one\n")
	h.model.push(calls(call{id: "run-1", name: "cell", args: "{}"}), text("done"))
	return h
}

func cellCompleted(e event.Event) bool {
	completed, ok := e.(event.ToolCompleted)
	return ok && completed.CallID == "run-1"
}

func requireCellVerdictsAfterReturn(t *testing.T, h *harness, thread *Thread, judge *judgeStub) {
	t.Helper()
	events := h.events(thread.ID())
	judged := judgedOf(events)
	if len(judged) != 2 {
		t.Fatalf("verdicts = %d, want the two cell edits", len(judged))
	}
	ids := []string{judged[0].CallID, judged[1].CallID}
	slices.Sort(ids)
	if !slices.Equal(ids, []string{"run-1/1", "run-1/2"}) {
		t.Fatalf("judged calls = %v, want the two nested edits", ids)
	}
	for _, entry := range judged {
		if entry.Verdict == reviewError || !judgedFollowsCompletion(events, entry.CallID) {
			t.Fatalf("verdict = %+v, want a non-error verdict after its completion", entry)
		}
	}
	requireCompletedOK(t, events, "run-1")
	requireReviewCounts(t, events, judge, 2, 1)
}

func requireNoCellReviewWhenOff(t *testing.T, h *harness, thread *Thread, judge *judgeStub) {
	t.Helper()
	events := h.events(thread.ID())
	if verdicts := len(judgedOf(events)); verdicts != 0 || judge.count() != 0 {
		t.Fatalf("verdicts and requests = %d, %d, want none with review off", verdicts, judge.count())
	}
	requireCompletedOK(t, events, "run-1")
	if got := h.read("a.txt"); got != "uno\n" {
		t.Fatalf("a.txt = %q, want the cell edit applied", got)
	}
}

func requireInterruptedCellReview(t *testing.T, events []event.Seqd) {
	t.Helper()
	judged := judgedOf(events)
	if len(judged) == 0 {
		t.Fatal("no verdict for the interrupted cell review")
	}
	for _, entry := range judged {
		if entry.Verdict != reviewError || !strings.Contains(entry.Error, "part 1 of 1: review: the turn was interrupted") {
			t.Fatalf("verdict = %+v, want the interrupt error", entry)
		}
	}
}
