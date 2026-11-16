package agent

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"math/rand/v2"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/decide"
)

type turnStub struct {
	scores  map[string]float64
	fail    error
	block   bool
	started chan struct{}
	once    sync.Once
}

func (s *turnStub) Ask(ctx context.Context, _ jsontext.Value, questions map[string]decide.Question) (decide.Result, error) {
	if _, turn := questions["incomplete"]; turn {
		if s.started != nil {
			s.once.Do(func() { close(s.started) })
		}
		if s.block {
			<-ctx.Done()
			return decide.Result{}, ctx.Err()
		}
		if s.fail != nil {
			return decide.Result{}, s.fail
		}
	}
	answers := make(map[string]decide.Answer, len(questions))
	for id := range questions {
		score, ok := s.scores[id]
		if !ok {
			score = 0.02
		}
		answers[id] = decide.Answer{Noul: &score}
	}
	return decide.Result{Model: "stub-judge", Answers: answers}, nil
}

func clientFor(c decide.Client) ClientFactory {
	return func(context.Context, string) (decide.Client, string, error) { return c, "stub-judge", nil }
}

func turnHarness(t *testing.T, c decide.Client, feedback []string) *harness {
	return newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.EditJudge = clientFor(c)
		h.cfg.Config.EditReview.Enabled = true
		h.cfg.EditFeedback = feedback
	})
}

func turnJudgedOf(events []event.Seqd) []event.TurnJudged {
	var out []event.TurnJudged
	for _, item := range events {
		if judged, ok := item.Event.(event.TurnJudged); ok {
			out = append(out, judged)
		}
	}
	return out
}

func turnStatesOf(j *judgeStub) []string {
	j.mu.Lock()
	defer j.mu.Unlock()
	var out []string
	for i, ids := range j.asked {
		if slices.Contains(ids, "incomplete") {
			out = append(out, j.states[i])
		}
	}
	return out
}

func turnAsksOf(j *judgeStub) [][]string {
	j.mu.Lock()
	defer j.mu.Unlock()
	var out [][]string
	for _, ids := range j.asked {
		if slices.Contains(ids, "incomplete") {
			out = append(out, ids)
		}
	}
	return out
}

func decodeTurnState(t *testing.T, raw string) turnState {
	t.Helper()
	var state turnState
	if err := json.Unmarshal([]byte(raw), &state); err != nil {
		t.Fatal(err)
	}
	return state
}

func TestTurnReviewRunsOnceAndTurnGoesOn(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{scores: map[string]float64{"sound": 0.2, "incomplete": 0.94}}
	h := judgedHarness(t, judge, []string{"incomplete"})
	h.write("a.txt", "one\n")
	h.model.push(calls(call{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}), text("first"), text("done"))
	thread := h.start()
	h.submit(thread, "fix it")
	h.waitIdle(thread)
	events := h.events(thread.ID())
	turns := turnJudgedOf(events)
	if len(turns) != 1 || turns[0].Verdict != reviewFlagged || !slices.Equal(turns[0].Spoken, []string{"incomplete"}) || !turns[0].Feedback {
		t.Fatalf("turn.judged = %+v, want one flagged verdict that speaks incomplete", turns)
	}
	if n := len(turnStatesOf(judge)); n != 1 {
		t.Fatalf("turn judge calls = %d, want 1", n)
	}
	if !slices.ContainsFunc(userTexts(h.model.request(2)), func(s string) bool { return strings.Contains(s, "<turn-review turn=") }) {
		t.Fatal("the request after the turn review holds no <turn-review> block")
	}
	if last := lastTurn(events); last.Status != statusCompleted {
		t.Fatalf("turn status = %q, want completed", last.Status)
	}
}

func TestTurnReviewSecondEndIsNotReviewed(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{scores: map[string]float64{"sound": 0.2, "incomplete": 0.94}}
	h := judgedHarness(t, judge, []string{"incomplete"})
	h.write("a.txt", "one\n")
	h.model.push(calls(call{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}), text("first"), text("second"))
	thread := h.start()
	h.submit(thread, "fix it")
	h.waitIdle(thread)
	if n := len(turnStatesOf(judge)); n != 1 {
		t.Fatalf("turn judge calls = %d, want 1", n)
	}
	if n := len(turnJudgedOf(h.events(thread.ID()))); n != 1 {
		t.Fatalf("turn.judged events = %d, want 1", n)
	}
	if n := h.model.count(); n != 3 {
		t.Fatalf("model requests = %d, want 3", n)
	}
}

type turnEndRow struct {
	name      string
	stub      *turnStub
	timeout   time.Duration
	fillSlots bool
	verdict   string
	errText   string
}

func TestTurnReviewEndsWithoutMessageUnlessItSpeaks(t *testing.T) {
	t.Parallel()
	rows := []turnEndRow{
		{name: "clean", stub: &turnStub{scores: map[string]float64{"sound": 0.02, "incomplete": 0.02}}, verdict: reviewClean},
		{name: "uncertain", stub: &turnStub{scores: map[string]float64{"sound": 0.2, "incomplete": 0.7}}, verdict: reviewUncertain},
		{name: "flagged but not spoken", stub: &turnStub{scores: map[string]float64{"sound": 0.5, "incomplete": 0.94}}, verdict: reviewFlagged},
		{name: "error", stub: &turnStub{fail: fmt.Errorf("boom")}, verdict: reviewError, errText: "part 1 of 1: boom"},
		{name: "slot wait times out", stub: &turnStub{}, timeout: 50 * time.Millisecond, fillSlots: true, verdict: reviewError, errText: "part 1 of 1: review waited 50ms for a judge slot"},
	}
	for _, row := range rows {
		runTurnEndRow(t, row)
	}
}

func runTurnEndRow(t *testing.T, row turnEndRow) {
	t.Helper()
	t.Run(row.name, func(t *testing.T) {
		t.Parallel()
		synctest.Test(t, func(t *testing.T) {
			h := turnHarness(t, row.stub, []string{"incomplete"})
			if row.timeout > 0 {
				h.host.reviewTimeout = row.timeout
			}
			if row.fillSlots {
				defer occupyReviewSlots(h)()
			}
			h.write("a.txt", "one\n")
			h.model.push(calls(call{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}), text("done"))
			thread := h.start()
			h.submit(thread, "fix it")
			h.waitIdle(thread)
			requireTurnEndsWithoutMessage(t, h, thread, row.verdict, row.errText)
		})
	})
}

func requireTurnEndsWithoutMessage(t *testing.T, h *harness, thread *Thread, verdict, errText string) {
	t.Helper()
	turns := turnJudgedOf(h.events(thread.ID()))
	if len(turns) != 1 || turns[0].Verdict != verdict || turns[0].Feedback || !strings.Contains(turns[0].Error, errText) {
		t.Fatalf("turn.judged = %+v, want verdict %s with error %q and no feedback", turns, verdict, errText)
	}
	if n := h.model.count(); n != 2 {
		t.Fatalf("model requests = %d, want 2: the turn must end without a review message", n)
	}
}

func TestTurnReviewRecoveryDoesNotReview(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.write("a.txt", "uno\n")
	h.seed(append(seededEdit(), event.ToolCompleted{CallID: "e1", OK: true, Output: "patched", Recovered: "none"})...)
	h.model.push(calls(call{id: "e2", name: "patch", args: patchArgs("a.txt", [2]string{"uno", "dos"})}), text("done"))
	events := h.recoverSession()
	if n := len(judgedOf(events)); n != 1 {
		t.Fatalf("tool.judged events = %d, want 1 for the new edit", n)
	}
	if n := len(turnJudgedOf(events)); n != 0 {
		t.Fatalf("turn.judged events = %d, want none on recovery", n)
	}
	if n := len(turnStatesOf(judge)); n != 0 {
		t.Fatalf("turn judge calls = %d, want none on recovery", n)
	}
}

func TestTurnReviewSkipsTurnWithoutEligibleEdit(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.model.push(text("done"))
	thread := h.start()
	h.submit(thread, "fix it")
	h.waitIdle(thread)
	if n := len(turnStatesOf(judge)); n != 0 {
		t.Fatalf("turn judge calls = %d, want 0", n)
	}
	if n := len(turnJudgedOf(h.events(thread.ID()))); n != 0 {
		t.Fatalf("turn.judged events = %d, want 0", n)
	}
	h.write("a.txt", "one\n")
	h.model.push(calls(call{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}), text("done"))
	h.submit(thread, "edit a.txt")
	h.waitIdle(thread)
	events := h.events(thread.ID())
	if n := len(turnStatesOf(judge)); n != 1 {
		t.Fatalf("turn judge calls = %d, want 1 for the editing turn", n)
	}
	requireReviewCounts(t, events, judge, 1, 1)
}

func lineEditRecords(t *testing.T, thread *Thread, pre, post string) []event.Seqd {
	t.Helper()
	before, err := thread.PutBlob([]byte(pre))
	if err != nil {
		t.Fatal(err)
	}
	after, err := thread.PutBlob([]byte(post))
	if err != nil {
		t.Fatal(err)
	}
	return []event.Seqd{
		{Seq: 1, Event: event.TurnStarted{TurnID: "t"}},
		{Seq: 2, Event: event.ToolStarted{CallID: "e1", TurnID: "t", Tool: "patch", Class: "edit", Files: []event.FileChange{
			{Path: "a.txt", PreSHA: "p", PostSHA: "q", PreBlob: &before, PostBlob: &after},
		}}},
		{Seq: 3, Event: event.ToolCompleted{CallID: "e1", OK: true}},
	}
}

func TestTurnReviewNeedsEligibleEdit(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.model.push(text("done"))
	thread := h.start()
	h.submit(thread, "fix it")
	h.waitIdle(thread)
	records := lineEditRecords(t, thread, "one\n", "uno\n")
	w := &worker{t: thread, turnID: "t"}
	if _, wanted := w.turnReviewWanted(view{records: records}); wanted {
		t.Fatal("turn review wanted without an eligible edit")
	}
	w.eligible = true
	if _, wanted := w.turnReviewWanted(view{records: records}); !wanted {
		t.Fatal("turn review not wanted with an eligible edit and a task")
	}
}

func TestTurnReviewSkipsTurnWithoutAddedLines(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.model.push(text("done"))
	thread := h.start()
	h.submit(thread, "fix it")
	h.waitIdle(thread)
	w := &worker{t: thread, turnID: "t", eligible: true}
	diff := w.collectTurnDiff(lineEditRecords(t, thread, "one\ntwo\n", "one\n"))
	if diff.err != nil {
		t.Fatal(diff.err)
	}
	if _, judged := w.judgeTurn(t.Context(), "fix it", diff, time.Now()); judged {
		t.Fatal("turn judged a turn with no added line")
	}
	if n := judge.count(); n != 0 {
		t.Fatalf("judge calls = %d, want 0 for a removal-only turn", n)
	}
}

func TestTurnReviewJudgesDeletedFile(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	pre, err := thread.PutBlob([]byte("one\n"))
	if err != nil {
		t.Fatal(err)
	}
	records := []event.Seqd{
		{Seq: 1, Event: event.TurnStarted{TurnID: "t"}},
		{Seq: 2, Event: event.ToolStarted{CallID: "e1", TurnID: "t", Tool: "patch", Class: "edit", Files: []event.FileChange{
			{Path: "gone.txt", PreSHA: "p", PreBlob: &pre},
		}}},
		{Seq: 3, Event: event.ToolCompleted{CallID: "e1", OK: true}},
	}
	w := &worker{t: thread, turnID: "t"}
	diff := w.collectTurnDiff(records)
	if diff.err != nil {
		t.Fatal(diff.err)
	}
	if !slices.Equal(diff.paths, []string{"gone.txt"}) || len(diff.skipped) != 0 {
		t.Fatalf("paths = %v skipped = %v, want [gone.txt] and none", diff.paths, diff.skipped)
	}
	if len(diff.files) != 1 || diff.files[0].path != "gone.txt" || diff.files[0].change != "delete" {
		t.Fatalf("files = %+v, want the deletion of gone.txt", diff.files)
	}
}

func TestTurnReviewNeedsTask(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.write("a.txt", "one\n")
	h.model.push(calls(call{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}), text("done"))
	thread := h.start()
	h.submit(thread, "")
	h.waitIdle(thread)
	if n := judge.count(); n != 1 {
		t.Fatalf("judge calls = %d, want 1: the edit only", n)
	}
	if n := len(turnStatesOf(judge)); n != 0 {
		t.Fatalf("turn judge calls = %d, want 0 without a task", n)
	}
}

func TestTurnReviewSkipsPathWithoutPostImage(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	pre, err := thread.PutBlob([]byte("one\n"))
	if err != nil {
		t.Fatal(err)
	}
	post, err := thread.PutBlob([]byte("uno\n"))
	if err != nil {
		t.Fatal(err)
	}
	records := []event.Seqd{
		{Seq: 1, Event: event.TurnStarted{TurnID: "t"}},
		{Seq: 2, Event: event.ToolStarted{CallID: "e1", TurnID: "t", Tool: "patch", Class: "edit", Files: []event.FileChange{
			{Path: "a.go", PreSHA: "p", PostSHA: "q", PreBlob: &pre, PostBlob: &post},
			{Path: "b.go", PreSHA: "p", PostSHA: "q", PreBlob: &pre},
		}}},
		{Seq: 3, Event: event.ToolCompleted{CallID: "e1", OK: true}},
	}
	w := &worker{t: thread, turnID: "t"}
	diff := w.collectTurnDiff(records)
	if diff.err != nil {
		t.Fatal(diff.err)
	}
	if !slices.Equal(diff.paths, []string{"a.go", "b.go"}) || !slices.Equal(diff.skipped, []string{"b.go"}) {
		t.Fatalf("paths = %v skipped = %v, want [a.go b.go] and [b.go]", diff.paths, diff.skipped)
	}
	if len(diff.files) != 1 || diff.files[0].path != "a.go" {
		t.Fatalf("files = %+v, want only a.go", diff.files)
	}
}

func TestTurnJudgedSkippedWithinPathCap(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	pre, err := thread.PutBlob([]byte("one\n"))
	if err != nil {
		t.Fatal(err)
	}
	post, err := thread.PutBlob([]byte("uno\n"))
	if err != nil {
		t.Fatal(err)
	}
	changes := make([]event.FileChange, 0, 70)
	for i := range 70 {
		change := event.FileChange{Path: fmt.Sprintf("f%02d.go", i), PreSHA: "p", PostSHA: "q", PreBlob: &pre, PostBlob: &post}
		if i == 63 || i == 64 {
			change.PostBlob = nil
		}
		changes = append(changes, change)
	}
	records := []event.Seqd{
		{Seq: 1, Event: event.TurnStarted{TurnID: "t"}},
		{Seq: 2, Event: event.ToolStarted{CallID: "e1", TurnID: "t", Tool: "patch", Class: "edit", Files: changes}},
		{Seq: 3, Event: event.ToolCompleted{CallID: "e1", OK: true}},
	}
	w := &worker{t: thread, turnID: "t"}
	diff := w.collectTurnDiff(records)
	if len(diff.paths) != event.MaxJudgedPaths {
		t.Fatalf("paths = %d, want %d", len(diff.paths), event.MaxJudgedPaths)
	}
	if !slices.Equal(diff.skipped, []string{"f63.go"}) {
		t.Fatalf("skipped = %v, want [f63.go]: the 65th path is beyond the cap", diff.skipped)
	}
}

func TestTurnReviewCumulativeDiff(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.write("a.txt", "one\nkeep\n")
	runRounds(t, h,
		[]call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "two"})}},
		[]call{{id: "e2", name: "patch", args: patchArgs("a.txt", [2]string{"two", "three"})}},
	)
	states := turnStatesOf(judge)
	if len(states) != 1 {
		t.Fatalf("turn judge calls = %d, want 1", len(states))
	}
	diff := decodeTurnState(t, states[0]).Files[0].Diff
	if !strings.Contains(diff, "-one\n") || !strings.Contains(diff, "+three\n") || strings.Contains(diff, "two") {
		t.Fatalf("diff = %q, want the change from one to three with no middle text", diff)
	}
}

func TestTurnReviewSendsChangedLinesOnly(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.write("a.txt", "one\ntwo\nthree\nfour\nfive\n")
	runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"three", "THREE"})}})
	states := turnStatesOf(judge)
	if len(states) != 1 {
		t.Fatalf("turn judge calls = %d, want 1", len(states))
	}
	diff := decodeTurnState(t, states[0]).Files[0].Diff
	lines := strings.Split(strings.TrimSuffix(diff, "\n"), "\n")
	for _, line := range lines[2:] {
		if !strings.HasPrefix(line, "@@") && !strings.HasPrefix(line, "-") && !strings.HasPrefix(line, "+") {
			t.Fatalf("diff line %q is unchanged text, want none", line)
		}
	}
	if !strings.Contains(diff, "-three\n") || !strings.Contains(diff, "+THREE\n") {
		t.Fatalf("diff = %q, want -three and +THREE", diff)
	}
}

func TestTurnReviewSharesRoomAcrossFiles(t *testing.T) {
	t.Parallel()
	small := strings.Repeat("a", 100)
	bigB := strings.Repeat(strings.Repeat("b", 99)+"\n", 51)
	bigC := strings.Repeat(strings.Repeat("c", 99)+"\n", 51)
	out := shareRoom([]string{small, bigB, bigC}, 6000)
	if out[0] != small {
		t.Fatalf("first diff = %d bytes, want kept whole", len(out[0]))
	}
	if n := jsonContentLen(out[1]); n > 2950 || !strings.HasPrefix(out[1], "bbb") || !strings.Contains(out[1], "[truncated: ") {
		t.Fatalf("second diff is %d encoded bytes with marker %v, want at most 2950 and a marker", n, strings.Contains(out[1], "[truncated: "))
	}
	if n := jsonContentLen(out[1]); n <= 2000 {
		t.Fatalf("second diff is %d encoded bytes, want its share of 2950 and not an equal third of the room", n)
	}
	if n := jsonContentLen(out[2]); n > 5900-jsonContentLen(out[1]) || !strings.Contains(out[2], "[truncated: ") {
		t.Fatalf("third diff is %d encoded bytes, want at most %d and a marker", n, 5900-jsonContentLen(out[1]))
	}
}

func randomTurnFile(rng *rand.Rand, alphabet []string, path string) fileDiff {
	var body strings.Builder
	for target := 1 + rng.IntN(9000); body.Len() < target; {
		body.WriteString(alphabet[rng.IntN(len(alphabet))])
	}
	return fileDiff{
		path: path, change: "update", kind: "code", header: "--- a/" + path + "\n+++ b/" + path + "\n",
		hunks: []hunk{{oldStart: 1, newStart: 1, lines: []string{"+" + body.String() + "\n"}}},
		added: 1,
	}
}

func randomTurnDiffs(rng *rand.Rand, alphabet []string) []fileDiff {
	for {
		var files []fileDiff
		total := 0
		for f := range 1 + rng.IntN(12) {
			file := randomTurnFile(rng, alphabet, fmt.Sprintf("f%d.go", f))
			total += len(file.hunks[0].lines[0])
			files = append(files, file)
		}
		if total >= 1000 && total <= 60000 {
			return files
		}
	}
}

func TestTurnRequestListsOnlyFilesWithChangedLines(t *testing.T) {
	t.Parallel()
	same := []byte("one\ntwo\n")
	files, err := diffFiles([]reviewFile{
		classifyReviewFile("named.go", "update", same, same),
		classifyReviewFile("a.go", "update", []byte("one\n"), same),
	}, 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(files) != 2 {
		t.Fatalf("diffFiles gave %d files, want both so the request filter is exercised", len(files))
	}
	input, added, err := turnRequest("task", files)
	if err != nil {
		t.Fatal(err)
	}
	state := decodeTurnState(t, string(input.state))
	if added != 1 || len(state.Files) != 1 || state.Files[0].Path != "a.go" {
		t.Fatalf("request lists %+v with %d added lines, want only a.go with 1", state.Files, added)
	}
	changed, _, err := turnRequest("task", files[1:])
	if err != nil || len(decodeTurnState(t, string(changed.state)).Files) != 1 {
		t.Fatalf("control request = %v, want one file", err)
	}
}

func TestTurnReviewRequestStaysUnderRequestLimit(t *testing.T) {
	t.Parallel()
	rng := rand.New(rand.NewPCG(3, 4))
	alphabet := []string{"a", "b", `"`, `\`, "\t", "é", " "}
	for range 200 {
		requireTurnRequestWithinLimit(t, rng, alphabet)
	}
	requireControlTurnRequestWhole(t, alphabet)
}

func requireTurnRequestWithinLimit(t *testing.T, rng *rand.Rand, alphabet []string) {
	t.Helper()
	files := randomTurnDiffs(rng, alphabet)
	input, _, err := turnRequest("task", files)
	if err != nil {
		t.Fatal(err)
	}
	size, err := decide.RequestSize(input.state, input.questions)
	if err != nil {
		t.Fatal(err)
	}
	if size > reviewRequestLimit {
		t.Fatalf("request size = %d, want at most %d", size, reviewRequestLimit)
	}
	state := decodeTurnState(t, string(input.state))
	if len(state.Files) != len(files) {
		t.Fatalf("state holds %d files, want %d", len(state.Files), len(files))
	}
	if input.truncated != anyTurnFileCut(state, files) {
		t.Fatalf("truncated = %t, want %t", input.truncated, anyTurnFileCut(state, files))
	}
}

func anyTurnFileCut(state turnState, files []fileDiff) bool {
	for i := range files {
		if state.Files[i].Diff != fileText(&files[i], files[i].hunks) {
			return true
		}
	}
	return false
}

func requireControlTurnRequestWhole(t *testing.T, alphabet []string) {
	t.Helper()
	control := randomTurnFile(rand.New(rand.NewPCG(5, 6)), alphabet, "control.go")
	input, _, err := turnRequest("task", []fileDiff{control})
	if err != nil {
		t.Fatal(err)
	}
	state := decodeTurnState(t, string(input.state))
	if input.truncated || state.Files[0].Diff != fileText(&control, control.hunks) {
		t.Fatalf("control truncated = %t, want the whole diff inside the limit", input.truncated)
	}
}

func TestTurnReviewContinuationIsOwnUnit(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.write("a.txt", "one\n")
	h.write("b.txt", "one\n")
	h.model.push(
		calls(call{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}),
		text("done"),
		calls(call{id: "e2", name: "patch", args: patchArgs("b.txt", [2]string{"one", "uno"})}),
		text("again"),
	)
	thread := h.start()
	h.submit(thread, "fix it")
	h.waitIdle(thread)
	if err := askErr(thread, func(a *actor) error {
		a.autoContinue = true
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := thread.Deliver(t.Context(), Note{Advisor: "reviewer", Severity: "blocker", Text: "keep going", ReviewSeq: 1, Delivery: DeliveryContinue}); err != nil {
		t.Fatal(err)
	}
	h.waitIdle(thread)
	turns := turnJudgedOf(h.events(thread.ID()))
	if len(turns) != 2 || !slices.Equal(turns[1].Paths, []string{"b.txt"}) {
		t.Fatalf("turn.judged = %+v, want two reviews and the second covering only b.txt", turns)
	}
	states := turnStatesOf(judge)
	if len(states) != 2 {
		t.Fatalf("turn judge calls = %d, want 2", len(states))
	}
	if files := decodeTurnState(t, states[1]).Files; len(files) != 1 || files[0].Path != "b.txt" {
		t.Fatalf("second review files = %+v, want only b.txt", files)
	}
}

func TestTurnReviewInterruptDuringWait(t *testing.T) {
	t.Parallel()
	stub := &turnStub{scores: map[string]float64{"sound": 0.2}, block: true, started: make(chan struct{})}
	h := turnHarness(t, stub, nil)
	h.write("a.txt", "one\n")
	h.model.push(calls(call{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}), text("done"))
	thread := h.start()
	var boundaries atomic.Int32
	if err := thread.SetBoundaryHook(func(b Boundary) Wait {
		if b.Kind == BoundaryAgentEnd && b.Ended == "" {
			boundaries.Add(1)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	h.submit(thread, "fix it")
	<-stub.started
	thread.Interrupt()
	h.waitIdle(thread)
	events := h.events(thread.ID())
	turns := turnJudgedOf(events)
	if len(turns) != 1 || turns[0].Verdict != reviewError || !strings.Contains(turns[0].Error, "part 1 of 1: review: the turn was interrupted") {
		t.Fatalf("turn.judged = %+v, want an error for the interrupted wait", turns)
	}
	if last := lastTurn(events); last.Status != statusInterrupted {
		t.Fatalf("turn status = %q, want interrupted", last.Status)
	}
	if n := boundaries.Load(); n != 0 {
		t.Fatalf("agent-end boundary ran %d times after the review, want none", n)
	}
}

func TestTurnReviewAsksSoundAndIncomplete(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, nil)
	h.write("a.txt", "one\n")
	runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
	asks := turnAsksOf(judge)
	if len(asks) != 1 || !slices.Equal(asks[0], []string{"incomplete", "sound"}) {
		t.Fatalf("turn questions = %v, want exactly [incomplete sound]", asks)
	}
}

type loopStep struct {
	name  string
	types []string
}

type orderedTurnEnd struct {
	baseExtension
	record func(string)
}

func (o *orderedTurnEnd) TurnEnd(context.Context, string, string) error {
	o.record("turn_end")
	return nil
}

func runLoopOrderReviewed(t *testing.T) ([]loopStep, []string) {
	t.Helper()
	return loopOrder(t, true)
}

func runLoopOrderUnreviewed(t *testing.T) ([]loopStep, []string) {
	t.Helper()
	return loopOrder(t, false)
}

func loopOrder(t *testing.T, enabled bool) ([]loopStep, []string) {
	t.Helper()
	var mu sync.Mutex
	var steps []loopStep
	var h *harness
	record := func(name, id string) {
		types := h.types(id)
		mu.Lock()
		defer mu.Unlock()
		steps = append(steps, loopStep{name: name, types: types})
	}
	h = newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.EditJudge = judgeFor(&judgeStub{})
		h.cfg.Config.EditReview.Enabled = enabled
		h.cfg.Extension = func(th *Thread) Extension {
			return &orderedTurnEnd{record: func(name string) { record(name, th.ID()) }}
		}
	})
	h.write("a.txt", "one\n")
	h.model.push(calls(call{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}), text("done"))
	thread := h.start()
	if err := thread.SetBoundaryHook(func(b Boundary) Wait {
		if b.Kind == BoundaryAgentEnd && b.Ended == "" {
			record("boundary", thread.ID())
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	h.submit(thread, "fix it")
	h.waitIdle(thread)
	mu.Lock()
	defer mu.Unlock()
	return slices.Clone(steps), h.types(thread.ID())
}

func TestTurnReviewLoopOrder(t *testing.T) {
	t.Parallel()
	t.Run("review on", func(t *testing.T) {
		t.Parallel()
		steps, final := runLoopOrderReviewed(t)
		requireLoopSteps(t, steps)
		requireJudgedBeforeEnd(t, steps, final)
	})
	t.Run("review off", func(t *testing.T) {
		t.Parallel()
		steps, final := runLoopOrderUnreviewed(t)
		requireLoopSteps(t, steps)
		requireNoTurnJudged(t, steps, final)
	})
}

func requireLoopSteps(t *testing.T, steps []loopStep) {
	t.Helper()
	if len(steps) != 2 || steps[0].name != "boundary" || steps[1].name != "turn_end" {
		t.Fatalf("steps = %+v, want the agent-end boundary and then the turn_end hook", steps)
	}
}

func requireJudgedBeforeEnd(t *testing.T, steps []loopStep, final []string) {
	t.Helper()
	for _, step := range steps {
		if !slices.Contains(step.types, "turn.judged") || slices.Contains(step.types, "turn.completed") {
			t.Fatalf("journal at %s = %v, want turn.judged and no turn.completed", step.name, step.types)
		}
	}
	if judged, completed := slices.Index(final, "turn.judged"), slices.Index(final, "turn.completed"); judged < 0 || completed < judged {
		t.Fatalf("final journal = %v, want turn.judged before turn.completed", final)
	}
}

func requireNoTurnJudged(t *testing.T, steps []loopStep, final []string) {
	t.Helper()
	for _, step := range steps {
		if slices.Contains(step.types, "turn.judged") {
			t.Fatalf("journal at %s = %v, want no turn.judged with review off", step.name, step.types)
		}
	}
	if slices.Contains(final, "turn.judged") {
		t.Fatalf("final journal = %v, want no turn.judged with review off", final)
	}
}

func TestTurnReviewTruncatedNeverSpeaks(t *testing.T) {
	t.Parallel()
	stub := &turnStub{scores: map[string]float64{"sound": 0.2, "incomplete": 0.94}}
	h := turnHarness(t, stub, []string{"incomplete"})
	h.write("a.txt", "one\n")
	big := strings.Repeat("line of text in the new file\n", 2000)
	h.model.push(calls(call{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", big})}), text("first"), text("done"))
	thread := h.start()
	h.submit(thread, "fix it")
	h.waitIdle(thread)
	turns := turnJudgedOf(h.events(thread.ID()))
	if len(turns) != 1 || turns[0].Verdict != reviewFlagged || !turns[0].Truncated || turns[0].Feedback || len(turns[0].Spoken) != 0 {
		t.Fatalf("turn.judged = %+v, want a flagged truncated verdict that does not speak", turns)
	}
	if n := h.model.count(); n != 2 {
		t.Fatalf("model requests = %d, want 2: a truncated turn adds no message", n)
	}
}

func TestTurnReviewMasksPaths(t *testing.T) {
	t.Parallel()
	store, err := auth.NewWithPatterns(t.TempDir(), []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	judge := &judgeStub{}
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.Auth = store
		h.cfg.EditJudge = judgeFor(judge)
		h.cfg.Config.EditReview.Enabled = true
	})
	const secret = "token-abcdefgh12"
	h.write(secret+"/a.txt", "one\n")
	thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs(secret+"/a.txt", [2]string{"one", "uno"})}})
	turns := turnJudgedOf(h.events(thread.ID()))
	if len(turns) != 1 || len(turns[0].Paths) != 1 {
		t.Fatalf("turn.judged = %+v, want one review with one path", turns)
	}
	if strings.Contains(turns[0].Paths[0], secret) || !strings.Contains(turns[0].Paths[0], "{{secret:") {
		t.Fatalf("path = %q, want the placeholder and no raw secret", turns[0].Paths[0])
	}
	encoded, err := event.Encode(turns[0])
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(encoded), secret) {
		t.Fatalf("turn.judged bytes hold the raw secret: %s", encoded)
	}
}

func editStatesOf(j *judgeStub) []string {
	j.mu.Lock()
	defer j.mu.Unlock()
	var out []string
	for i, ids := range j.asked {
		if !slices.Contains(ids, "incomplete") {
			out = append(out, j.states[i])
		}
	}
	return out
}
