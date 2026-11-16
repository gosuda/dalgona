package agent

import (
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/read"
)

func TestValidateReasoningEffort(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name      string
		requested string
		supported []string
		wantErr   string
	}{
		{name: "empty passes", requested: "", supported: []string{"low", "high"}},
		{name: "default passes", requested: "default", supported: []string{"low", "high"}},
		{name: "supported passes", requested: "high", supported: []string{"low", "high"}},
		{name: "unknown capabilities pass", requested: "low", supported: nil},
		{name: "declared empty capabilities pass", requested: "low", supported: []string{}},
		{
			name:      "unsupported is refused",
			requested: "xhigh",
			supported: []string{"low", "medium", "high"},
			wantErr:   "does not support reasoning effort xhigh; supported: [low medium high]",
		},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			err := ValidateReasoningEffort(test.requested, test.supported)
			if test.wantErr == "" {
				if err != nil {
					t.Fatalf("ValidateReasoningEffort(%q, %v) = %v, want nil", test.requested, test.supported, err)
				}
				return
			}
			if err == nil || err.Error() != test.wantErr {
				t.Fatalf("ValidateReasoningEffort(%q, %v) = %v, want %q", test.requested, test.supported, err, test.wantErr)
			}
		})
	}
}

func TestExplicitReasoningEffortRefusesUnsupported(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.host.models["openai"] = modelCache{models: []model.Model{{ID: "gpt-test", Context: 128000, Efforts: []string{"low", "medium", "high"}}}}
	thread := h.start()
	h.submit(thread, "unset effort")
	h.waitIdle(thread)
	if reasoning := h.model.request(0)["reasoning"]; reasoning != nil {
		t.Fatalf("unset effort reasoning = %v, want nil", reasoning)
	}
	submitEffort(t, thread, "supported turn effort", "high")
	h.waitIdle(thread)
	if got := reasoningEffort(h.model.request(1)); got != "high" {
		t.Fatalf("supported explicit effort = %v, want high", got)
	}
	before := h.model.count()
	submitEffort(t, thread, "unsupported turn effort", "xhigh")
	h.waitIdle(thread)
	events := h.events(thread.ID())
	turn := lastTurn(events)
	want := "model gpt-test does not support reasoning effort xhigh; supported: [low medium high]"
	if turn.Status != statusFailed || turn.Error != want {
		t.Fatalf("turn = %s %q, want failed %q", turn.Status, turn.Error, want)
	}
	if got := h.model.count(); got != before {
		t.Fatalf("model requests = %d, want %d: no network call for a refused effort", got, before)
	}
	requireTurnEfforts(t, events, "", "high")
}

func submitEffort(t *testing.T, thread *Thread, text, effort string) {
	t.Helper()
	if err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: text, Effort: effort}); err != nil {
		t.Fatal(err)
	}
}

func requireTurnEfforts(t *testing.T, events []event.Seqd, want ...string) {
	t.Helper()
	var got []string
	for _, record := range events {
		if effort, ok := record.Event.(event.TurnEffort); ok {
			got = append(got, effort.Effort)
		}
	}
	if !slices.Equal(got, want) {
		t.Fatalf("resolved efforts = %v, want %v with no refused resolution", got, want)
	}
}

func TestUnknownEffortCapabilitiesOmitEffort(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	h.submit(thread, "plain turn")
	h.waitIdle(thread)
	if err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "unknown capabilities turn", Effort: "high"}); err != nil {
		t.Fatal(err)
	}
	h.waitIdle(thread)
	turn := lastTurn(h.events(thread.ID()))
	if turn.Status != statusCompleted {
		t.Fatalf("turn = %s %q, want completed with the effort omitted", turn.Status, turn.Error)
	}
	if reasoning := h.model.request(1)["reasoning"]; reasoning != nil {
		t.Fatalf("unknown capabilities reasoning = %v, want nil", reasoning)
	}
}

func seededEffortTurn(effort string) []event.Event {
	return []event.Event{
		event.TurnStarted{TurnID: "t"},
		event.UserMessage{ItemID: "u", TurnID: "t", Text: "resume me", Kind: "prompt", Effort: effort},
	}
}

func recoverWithEfforts(t *testing.T, h *harness, efforts []string) event.TurnCompleted {
	t.Helper()
	host := h.newHost()
	host.models["openai"] = modelCache{models: []model.Model{{ID: "gpt-test", Context: 128000, Efforts: efforts}}}
	if err := host.Recover(t.Context()); err != nil {
		t.Fatal(err)
	}
	thread, err := host.Resume(t.Context(), crashSession)
	if err != nil {
		t.Fatal(err)
	}
	h.waitIdle(thread)
	return lastTurn(h.events(crashSession))
}

func TestReplayOmitsUnsupportedStoredEffort(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.seed(seededEffortTurn("xhigh")...)
	if turn := recoverWithEfforts(t, h, []string{"low", "medium", "high"}); turn.Status != statusCompleted {
		t.Fatalf("recovered turn = %s %q, want completed with the effort omitted", turn.Status, turn.Error)
	}
	if reasoning := h.model.request(0)["reasoning"]; reasoning != nil {
		t.Fatalf("replayed reasoning = %v, want nil", reasoning)
	}
	if !strings.Contains(h.logs.String(), "reasoning effort xhigh is not supported by gpt-test; sent without effort") {
		t.Fatalf("logs lack the omission notice:\n%s", h.logs.String())
	}
}

func TestReplayKeepsSupportedStoredEffort(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.seed(seededEffortTurn("high")...)
	if turn := recoverWithEfforts(t, h, []string{"low", "medium", "high"}); turn.Status != statusCompleted {
		t.Fatalf("recovered turn = %s %q, want completed", turn.Status, turn.Error)
	}
	if got := reasoningEffort(h.model.request(0)); got != "high" {
		t.Fatalf("replayed reasoning = %v, want high", got)
	}
	if strings.Contains(h.logs.String(), "sent without effort") {
		t.Fatalf("logs hold an omission notice for a supported effort:\n%s", h.logs.String())
	}
}

func startEffortChild(h *harness, declared []string) *Thread {
	h.t.Helper()
	h.host.models["openai"] = modelCache{models: []model.Model{{ID: "gpt-test", Context: 128000, Efforts: declared}}}
	parent := h.start()
	child, err := h.host.Start(h.t.Context(), ThreadSpec{
		Parent: parent.ID(), Advisor: "advisor", Tools: []tool.Tool{read.New()}, System: "advisor",
	})
	if err != nil {
		h.t.Fatal(err)
	}
	return child
}

func TestChildThreadFallsBackToNearestLowerDeclaredEffort(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name      string
		requested string
		declared  []string
		want      string
	}{
		{name: "skips an undeclared level downward", requested: "medium", declared: []string{"low", "high"}, want: "low"},
		{name: "keeps a declared level", requested: "medium", declared: []string{"low", "medium"}, want: "medium"},
		{name: "sends nothing without a lower level", requested: "medium", declared: []string{"high"}, want: ""},
		{name: "picks the highest lower level in any list order", requested: "xhigh", declared: []string{"max", "low", "high"}, want: "high"},
		{name: "reaches minimal", requested: "medium", declared: []string{"high", "minimal"}, want: "minimal"},
		{name: "never rises to a higher level", requested: "low", declared: []string{"medium", "high"}, want: ""},
		{name: "declares no level", requested: "medium", declared: nil, want: ""},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			requireChildEffort(t, test.declared, test.requested, test.want)
		})
	}
}

func requireChildEffort(t *testing.T, declared []string, requested, want string) {
	t.Helper()
	h := newHarness(t)
	child := startEffortChild(h, declared)
	if err := child.Submit(t.Context(), Input{Kind: InputPrompt, Text: "review", Effort: requested}); err != nil {
		t.Fatal(err)
	}
	h.waitIdle(child)
	if turn := lastTurn(h.events(child.ID())); turn.Status != statusCompleted {
		t.Fatalf("child turn = %s %q, want completed", turn.Status, turn.Error)
	}
	requireRequestEffort(t, h.model.request(0), want)
}

func requireRequestEffort(t *testing.T, request map[string]any, want string) {
	t.Helper()
	if want == "" {
		if request["reasoning"] != nil {
			t.Fatalf("reasoning = %v, want no effort member", request["reasoning"])
		}
		return
	}
	if got := reasoningEffort(request); got != want {
		t.Fatalf("effort = %v, want %s", got, want)
	}
}

func TestTopLevelTurnRefusesUndeclaredMediumEffort(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.host.models["openai"] = modelCache{models: []model.Model{{ID: "gpt-test", Context: 128000, Efforts: []string{"low", "high"}}}}
	thread := h.start()
	if err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "undeclared medium", Effort: "medium"}); err != nil {
		t.Fatal(err)
	}
	h.waitIdle(thread)
	turn := lastTurn(h.events(thread.ID()))
	want := "model gpt-test does not support reasoning effort medium; supported: [low high]"
	if turn.Status != statusFailed || turn.Error != want {
		t.Fatalf("turn = %s %q, want failed %q", turn.Status, turn.Error, want)
	}
	if got := h.model.count(); got != 0 {
		t.Fatalf("model requests = %d, want 0: a top-level refusal makes no network call", got)
	}
}
