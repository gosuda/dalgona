package agent

import (
	"reflect"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
)

const (
	editFeedbackBlock = "<edit-review call=\"e1\" paths=\"a.txt\">\nAn automatic review of this edit found possible problems:\n- The edit may remove code whose behaviour no added line restores.\nCheck the changed lines. If a problem is real, fix it. If not, ignore this review.\n</edit-review>"
	turnFeedbackBlock = "<turn-review turn=\"t\">\nAn automatic review of this turn found possible problems:\n- The task may name a change that the diff does not make, or leave a placeholder where the task needs working code.\nCheck the changed files. If a problem is real, fix it. If not, ignore this review.\n</turn-review>"
)

func inputText(item map[string]any) string {
	content, _ := item["content"].([]any)
	var out []string
	for _, part := range content {
		if object, ok := part.(map[string]any); ok {
			if value, ok := object["text"].(string); ok {
				out = append(out, value)
			}
		}
	}
	return strings.Join(out, "")
}

func isCallOutput(item map[string]any, callID string) bool {
	return item["type"] == "function_call_output" && item["call_id"] == callID
}

func TestEditReviewFeedbackFollowsToolResults(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{scores: map[string]float64{"sound": 0.2, "lost_code": 0.94}}
	h := judgedHarness(t, judge, []string{"lost_code"})
	h.write("a.txt", "one\ntwo\n")
	runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
	items := inputItems(h.model.request(1))
	index := slices.IndexFunc(items, func(item map[string]any) bool { return isCallOutput(item, "e1") })
	if index < 0 || index+1 >= len(items) {
		t.Fatalf("request 1 holds no output after e1 and no block: %v", items)
	}
	if got := inputText(items[index+1]); items[index+1]["role"] != "user" || got != editFeedbackBlock {
		t.Fatalf("block after the results = %q, want %q", got, editFeedbackBlock)
	}
}

func TestEditReviewCleanVerdictLeavesRequestUnchanged(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, []string{"lost_code"})
	h.write("a.txt", "one\ntwo\n")
	runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
	items := inputItems(h.model.request(1))
	if last := items[len(items)-1]; !isCallOutput(last, "e1") {
		t.Fatalf("last input item = %v, want the output of e1", last)
	}
	for _, text := range userTexts(h.model.request(1)) {
		if strings.Contains(text, "<edit-review") {
			t.Fatalf("clean verdict added a block: %q", text)
		}
	}
}

func TestEditReviewFeedbackKeepsRequestPrefix(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{scores: map[string]float64{"sound": 0.2, "lost_code": 0.94}}
	h := judgedHarness(t, judge, []string{"lost_code"})
	h.write("a.txt", "one\ntwo\n")
	h.model.push(
		calls(call{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}),
		calls(call{id: "e2", name: "patch", args: patchArgs("a.txt", [2]string{"two", "dos"})}),
		text("done"),
	)
	thread := h.start()
	h.submit(thread, "fix it")
	h.waitIdle(thread)
	before, after := inputItems(h.model.request(1)), inputItems(h.model.request(2))
	if len(after) < len(before) {
		t.Fatalf("request 2 has %d items, request 1 has %d", len(after), len(before))
	}
	for i := range before {
		if !reflect.DeepEqual(before[i], after[i]) {
			t.Fatalf("item %d changed between requests:\n%v\n%v", i, before[i], after[i])
		}
	}
}

func TestTurnReviewFeedbackIsLastUserMessage(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalYolo))
	h.seed(append(openTurn("fix it"),
		event.AssistantMessage{ItemID: "a-t", TurnID: "t", StopReason: "stop", Text: "done"},
		event.TurnJudged{
			TurnID: "t", By: "judge:stub-judge", Verdict: reviewFlagged, Paths: []string{"a.txt"},
			Scores: map[string]float64{"sound": 0.2, "incomplete": 0.94}, Flags: []string{"incomplete"},
			Spoken: []string{"incomplete"}, Feedback: true, ElapsedMS: 1,
		},
	)...)
	h.model.push(text("done"))
	h.recoverSession()
	texts := userTexts(h.model.request(0))
	if len(texts) == 0 || texts[len(texts)-1] != turnFeedbackBlock {
		t.Fatalf("last user text = %q, want %q", texts, turnFeedbackBlock)
	}
}

func TestFeedbackBuiltFromJournaledSpoken(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := judgedHarness(t, judge, []string{"placeholder"})
	h.seed(append(seededEdit(),
		event.ToolCompleted{CallID: "e1", OK: true, Output: "patched", Recovered: "none"},
		event.ToolJudged{
			CallID: "e1", TurnID: "t", By: "judge:stub-judge", Verdict: reviewFlagged, Paths: []string{"a.txt"},
			Scores: map[string]float64{"sound": 0.2, "lost_code": 0.94}, Flags: []string{"lost_code"},
			Spoken: []string{"lost_code"}, Feedback: true, Parts: 1, ElapsedMS: 1,
		},
	)...)
	h.model.push(text("done"))
	h.recoverSession()
	joined := strings.Join(userTexts(h.model.request(0)), "\n")
	if !strings.Contains(joined, "The edit may remove code whose behaviour no added line restores.") {
		t.Fatalf("rebuilt request lacks the journaled sentence: %q", joined)
	}
	if strings.Contains(joined, "An added line may be a placeholder instead of working code.") {
		t.Fatalf("rebuilt request used the current speaker set: %q", joined)
	}
}

func TestReviewFeedbackEscapesAttributes(t *testing.T) {
	t.Parallel()
	block := reviewFeedback(event.ToolJudged{CallID: `call"7<x>`, Paths: []string{"a&b.go"}, Spoken: []string{"lost_code"}, Feedback: true})
	for _, want := range []string{`call="call&#34;7&lt;x&gt;"`, `paths="a&amp;b.go"`} {
		if !strings.Contains(block, want) {
			t.Fatalf("block %q lacks %s", block, want)
		}
	}
}

func TestReviewFeedbackCoverageLineOnPartialReview(t *testing.T) {
	t.Parallel()
	whole := reviewFeedback(event.ToolJudged{CallID: "e1", Paths: []string{"a.go"}, Spoken: []string{"lost_code"}, Feedback: true})
	partial := reviewFeedback(event.ToolJudged{CallID: "e1", Paths: []string{"a.go"}, Spoken: []string{"lost_code"}, Feedback: true, Truncated: true})
	const line = "This review covered part of the edit.\n"
	if strings.Contains(whole, line) {
		t.Fatalf("whole review carries the coverage line: %q", whole)
	}
	if !strings.Contains(partial, line) {
		t.Fatalf("partial review lacks the coverage line: %q", partial)
	}
}
