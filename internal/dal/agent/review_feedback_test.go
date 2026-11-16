package agent

import (
	"encoding/xml"
	"errors"
	"io"
	"maps"
	"reflect"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
)

const (
	lostCodeSentence   = "The edit may remove code whose behaviour no added line restores."
	incompleteSentence = "The task may name a change that the diff does not make, or leave a placeholder where the task needs working code."
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

func bulletsOf(text string) []string {
	var out []string
	for line := range strings.SplitSeq(text, "\n") {
		if rest, ok := strings.CutPrefix(line, "- "); ok {
			out = append(out, rest)
		}
	}
	return out
}

func TestEditReviewFeedbackFollowsToolResults(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{scores: map[string]float64{"sound": 0.2, "lost_code": 0.94}}
	h := judgedHarness(t, judge, []string{"lost_code"})
	h.write("a.txt", "one\ntwo\n")
	runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.txt", [2]string{"one", "uno"})}})
	items := inputItems(h.model.request(1))
	index := slices.IndexFunc(items, func(item map[string]any) bool { return isCallOutput(item, "e1") })
	if index < 0 || index+2 != len(items) {
		t.Fatalf("request holds %d items after the result of e1, want exactly one", len(items)-index-1)
	}
	block := items[index+1]
	if block["role"] != "user" {
		t.Fatalf("block role = %v, want user", block["role"])
	}
	text := inputText(block)
	if !strings.HasPrefix(text, `<edit-review call="e1" paths="a.txt">`) || !strings.HasSuffix(text, "</edit-review>") {
		t.Fatalf("block = %q, want the edit-review tags", text)
	}
	if got := bulletsOf(text); !slices.Equal(got, []string{lostCodeSentence}) {
		t.Fatalf("bullets = %v, want one lost_code bullet", got)
	}
}

func TestEditReviewNonSpeakingFlagAddsNoMessage(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{scores: map[string]float64{"sound": 0.2, "needless_layer": 0.94}}
	h := judgedHarness(t, judge, []string{"lost_code"})
	h.write("a.go", "one\ntwo\n")
	thread := runRounds(t, h, []call{{id: "e1", name: "patch", args: patchArgs("a.go", [2]string{"one", "uno"})}})
	judged := judgedOf(h.events(thread.ID()))
	if len(judged) != 1 || judged[0].Verdict != reviewFlagged || judged[0].Feedback || len(judged[0].Spoken) != 0 {
		t.Fatalf("judged = %+v, want a flagged verdict that does not speak", judged)
	}
	items := inputItems(h.model.request(1))
	if last := items[len(items)-1]; !isCallOutput(last, "e1") {
		t.Fatalf("last input item = %v, want the output of e1 and no review message", last)
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
	rows := []struct {
		name   string
		speaks bool
	}{
		{name: "speaking", speaks: true},
		{name: "not speaking"},
	}
	for _, row := range rows {
		t.Run(row.name, func(t *testing.T) {
			t.Parallel()
			verdict := event.TurnJudged{
				TurnID: "t", By: "judge:stub-judge", Verdict: reviewFlagged, Paths: []string{"a.txt"},
				Scores: map[string]float64{"sound": 0.2, "incomplete": 0.94}, Flags: []string{"incomplete"}, ElapsedMS: 1,
			}
			if row.speaks {
				verdict.Spoken, verdict.Feedback = []string{"incomplete"}, true
			}
			h := newHarness(t, withApproval(config.ApprovalYolo))
			h.seed(append(openTurn("fix it"),
				event.AssistantMessage{ItemID: "a-t", TurnID: "t", StopReason: "stop", Text: "done"},
				verdict,
			)...)
			if row.speaks {
				h.model.push(text("done"))
			}
			events := h.recoverSession()
			if row.speaks {
				requireTurnReviewFeedbackSpeaks(t, h)
				return
			}
			requireSilentTurnReviewAddsNoMessage(t, h, events)
		})
	}
}

func requireTurnReviewFeedbackSpeaks(t *testing.T, h *harness) {
	t.Helper()
	items := inputItems(h.model.request(0))
	last := items[len(items)-1]
	block := inputText(last)
	if last["role"] != "user" || !strings.HasPrefix(block, `<turn-review turn="t">`) || !strings.HasSuffix(block, "</turn-review>") {
		t.Fatalf("last item = %v, want one turn-review user message", last)
	}
	if got := bulletsOf(block); !slices.Equal(got, []string{incompleteSentence}) {
		t.Fatalf("bullets = %v, want one incomplete bullet", got)
	}
}

func requireSilentTurnReviewAddsNoMessage(t *testing.T, h *harness, events []event.Seqd) {
	t.Helper()
	if requests := h.model.count(); requests != 0 {
		t.Fatalf("model requests = %d, want none for a non-speaking verdict", requests)
	}
	verdictAt := slices.IndexFunc(events, func(item event.Seqd) bool {
		_, ok := item.Event.(event.TurnJudged)
		return ok
	})
	added := verdictAt >= 0 && slices.ContainsFunc(events[verdictAt+1:], func(item event.Seqd) bool {
		_, ok := item.Event.(event.UserMessage)
		return ok
	})
	if verdictAt < 0 || added {
		t.Fatalf("a non-speaking verdict added a message after the verdict: %v", h.types(crashSession))
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
	attrs := parseReviewBlock(t, block)
	if attrs["call"] != `call"7<x>` || attrs["paths"] != "a&b.go" {
		t.Fatalf("attributes = %v, want the escaped values decoded", attrs)
	}
	control := reviewFeedback(event.ToolJudged{CallID: "e1", Paths: []string{`a"><b>`}, Spoken: []string{"lost_code"}, Feedback: true})
	if got := parseReviewBlock(t, control); got["paths"] != `a"><b>` {
		t.Fatalf("control attributes = %v, want one edit-review element", got)
	}
}

func parseReviewBlock(t *testing.T, block string) map[string]string {
	t.Helper()
	decoder := xml.NewDecoder(strings.NewReader(block))
	starts, ends := 0, 0
	attrs := map[string]string{}
	for {
		token, err := decoder.Token()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			t.Fatalf("parse review block: %v", err)
		}
		switch value := token.(type) {
		case xml.StartElement:
			starts++
			maps.Copy(attrs, reviewBlockAttrs(t, value))
		case xml.EndElement:
			ends++
		}
	}
	if starts != 1 || ends != 1 {
		t.Fatalf("block holds %d start and %d end elements, want one of each", starts, ends)
	}
	return attrs
}

func reviewBlockAttrs(t *testing.T, value xml.StartElement) map[string]string {
	t.Helper()
	if value.Name.Local != "edit-review" {
		t.Fatalf("element %q is not edit-review", value.Name.Local)
	}
	attrs := map[string]string{}
	for _, attr := range value.Attr {
		attrs[attr.Name.Local] = attr.Value
	}
	return attrs
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
