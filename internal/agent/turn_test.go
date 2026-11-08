package agent

import (
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/model"
)

func TestTurnReadsThenAnswers(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.write("a.txt", "alpha line\n")
	h.model.push(calls(call{id: "c1", name: "read", args: `{"path":"a.txt"}`}), text("the file says alpha"))
	thread := h.start()
	h.submit(thread, "what is in a.txt?")
	h.waitIdle(thread)
	want := []string{
		"session.created", "turn.started", "user.message", "assistant.message",
		"approval.requested", "approval.resolved", "tool.started", "tool.completed",
		"assistant.message", "turn.completed",
	}
	if got := h.types(thread.ID()); !slices.Equal(got, want) {
		t.Fatalf("events = %v\nwant     %v\nturn %+v", got, want, lastTurn(h.events(thread.ID())))
	}
	events := h.events(thread.ID())
	if turn := lastTurn(events); turn.Status != statusCompleted {
		t.Fatalf("turn = %+v", turn)
	}
	output := callOutputs(h.model.request(1))["c1"]
	if !strings.Contains(output, "alpha line") {
		t.Fatalf("second request tool output = %q", output)
	}
	resolved := events[5].Event.(event.ApprovalResolved)
	if resolved.Decision != "accept" || resolved.By != "mode:auto" {
		t.Fatalf("read approval = %+v", resolved)
	}
}

func TestSystemPromptIsStableAndNoticeRidesNewestMessage(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.host.models["openai"] = []model.Model{{ID: "gpt-test", Context: 128000, Efforts: []string{"low", "medium", "high"}}}
	thread := h.start()
	h.submit(thread, "first question")
	h.waitIdle(thread)
	h.submit(thread, "ultrathink about the second question")
	h.waitIdle(thread)
	first, second := h.model.request(0), h.model.request(1)
	if first["instructions"] == nil || first["instructions"] != second["instructions"] {
		t.Fatalf("system prompts differ:\n%v\n---\n%v", first["instructions"], second["instructions"])
	}
	texts := userTexts(second)
	if len(texts) != 2 || strings.Contains(texts[0], "<system-notice>") || !strings.Contains(texts[1], "<system-notice>") {
		t.Fatalf("user texts = %q", texts)
	}
	reasoning, _ := second["reasoning"].(map[string]any)
	if first["reasoning"] != nil || reasoning["effort"] != "high" {
		t.Fatalf("reasoning first=%v second=%v", first["reasoning"], second["reasoning"])
	}
}

func TestSteerArrivesAfterToolResultBeforeNextCall(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.write("a.txt", "alpha\n")
	gate := make(chan struct{})
	h.model.push(calls(call{id: "c1", name: "read", args: `{"path":"a.txt"}`}), response{body: sseText("ok"), gate: gate})
	thread := h.start()
	h.submit(thread, "read a.txt")
	h.waitRequests(2)
	if err := thread.Submit(t.Context(), Input{Kind: InputSteer, Text: "also mention beta"}); err != nil {
		t.Fatal(err)
	}
	close(gate)
	h.waitIdle(thread)
	if h.model.count() != 3 {
		t.Fatalf("requests = %d, want 3 (the late steer needs its own request)", h.model.count())
	}
	items := inputItems(h.model.request(2))
	outputAt, steerAt := itemIndex(items, func(item map[string]any) bool { return item["type"] == "function_call_output" }),
		itemIndex(items, func(item map[string]any) bool { return strings.Contains(textOf(item), "also mention beta") })
	if outputAt < 0 || steerAt != len(items)-1 || steerAt < outputAt {
		t.Fatalf("steer at %d, tool output at %d, items %d", steerAt, outputAt, len(items))
	}
}

func itemIndex(items []map[string]any, match func(map[string]any) bool) int {
	found := -1
	for i, item := range items {
		if match(item) {
			found = i
		}
	}
	return found
}

func lastText(t *testing.T, texts []string) string {
	t.Helper()
	if len(texts) == 0 {
		t.Fatal("request carries no user text")
	}
	return texts[len(texts)-1]
}

func textOf(item map[string]any) string {
	content, _ := item["content"].([]any)
	var out strings.Builder
	for _, part := range content {
		if object, ok := part.(map[string]any); ok {
			value, _ := object["text"].(string)
			out.WriteString(value)
		}
	}
	return out.String()
}

func TestLoopGuardRefusesFifthRepeat(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.write("a", "x\n")
	for i := range 5 {
		h.model.push(calls(call{id: "r" + string(rune('1'+i)), name: "read", args: `{"path":"a"}`}))
	}
	h.model.push(text("stopping"))
	thread := h.start()
	h.submit(thread, "loop")
	h.waitIdle(thread)
	outputs := completedOutputs(h.events(thread.ID()))
	for _, id := range []string{"r1", "r2", "r3", "r4"} {
		if !outputs[id].OK {
			t.Fatalf("%s = %+v", id, outputs[id])
		}
	}
	if fifth := outputs["r5"]; fifth.OK || fifth.Output != loopGuardMessage {
		t.Fatalf("fifth = %+v", fifth)
	}
}

func TestEmptyResponseRetriesTwiceThenFails(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	empty := response{body: completed()}
	h.model.push(empty, empty, empty)
	thread := h.start()
	h.submit(thread, "hello")
	h.waitIdle(thread)
	events := h.events(thread.ID())
	retries := 0
	for _, item := range events {
		if retry, ok := item.Event.(event.ModelRetry); ok && retry.Status == 0 {
			retries++
		}
	}
	turn := lastTurn(events)
	if retries != 2 || turn.Status != statusFailed || turn.Error != "empty model response" {
		t.Fatalf("retries = %d, turn = %+v", retries, turn)
	}
}

func TestMentionsAttachWorkspaceFilesOnly(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.write("go.mod", "module example\n")
	thread := h.start()
	h.submit(thread, "explain @go.mod and @../outside")
	h.waitIdle(thread)
	texts := userTexts(h.model.request(0))
	if len(texts) != 1 || !strings.Contains(texts[0], `<file path="go.mod">`) || !strings.Contains(texts[0], "module example") {
		t.Fatalf("texts = %q", texts)
	}
	if strings.Contains(texts[0], `<file path="../outside">`) || !strings.Contains(texts[0], "@../outside") {
		t.Fatalf("outside mention attached: %q", texts[0])
	}
}

func TestNestedInstructionsAppendOnce(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.write("AGENTS.md", "root rules\n")
	h.write("sub/AGENTS.md", "sub rules\n")
	h.write("sub/x.go", "package sub\n")
	h.model.push(
		calls(call{id: "c1", name: "read", args: `{"path":"sub/x.go"}`}),
		calls(call{id: "c2", name: "read", args: `{"path":"sub/x.go"}`}),
		text("done"),
	)
	thread := h.start()
	h.submit(thread, "read twice")
	h.waitIdle(thread)
	outputs := completedOutputs(h.events(thread.ID()))
	if !strings.Contains(outputs["c1"].Output, "sub rules") {
		t.Fatalf("first read = %q", outputs["c1"].Output)
	}
	if strings.Contains(outputs["c2"].Output, "sub rules") {
		t.Fatalf("second read repeated instructions: %q", outputs["c2"].Output)
	}
	if !strings.Contains(h.model.request(0)["instructions"].(string), "root rules") {
		t.Fatal("root AGENTS.md missing from the system prompt")
	}
}

func TestCustomKeywordAndProductScopedBuiltins(t *testing.T) {
	t.Parallel()
	custom := newHarness(t, func(h *harness) {
		h.cfg.Config.Keywords = map[string]config.KeywordSpec{"review": {Text: "REVIEW NOTICE BODY", Enabled: true, Hue: [2]int{1, 2}}}
	})
	thread := custom.start()
	custom.submit(thread, "please review this")
	custom.waitIdle(thread)
	if texts := userTexts(custom.model.request(0)); !strings.Contains(texts[0], "<system-notice>\nREVIEW NOTICE BODY\n</system-notice>") {
		t.Fatalf("custom notice missing: %q", texts)
	}
	dal := newHarness(t, func(h *harness) { h.cfg.Config.Product, h.cfg.Product = config.ProductDal, config.ProductDal })
	thread = dal.start()
	dal.submit(thread, "jevify the results")
	dal.waitIdle(thread)
	if texts := userTexts(dal.model.request(0)); strings.Contains(texts[0], "<system-notice>") {
		t.Fatalf("jevify fired in dal: %q", texts)
	}
}
