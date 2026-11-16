package rpc

import (
	"log/slog"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
)

type costSink struct{ usages []ThreadTokenUsage }

func (s *costSink) turn(string, Turn)          {}
func (s *costSink) item(string, string, *Item) {}
func (s *costSink) notify(method string, build func() writer) {
	if method != "thread/tokenUsage/updated" {
		return
	}
	if update, ok := build().(ThreadTokenUsageUpdatedNotification); ok {
		s.usages = append(s.usages, update.TokenUsage)
	}
}

func costTracker(prices map[string]model.Price, sessionModel string) (*tracker, *costSink) {
	noKeywords := func(string, string, agent.InputKind) []agent.KeywordMatch { return nil }
	tr := newTracker("t1", nil, nil, noKeywords, slog.New(slog.DiscardHandler))
	sink := &costSink{}
	tr.sink = sink
	if prices != nil {
		tr.price = func(id string) (model.Price, bool) {
			price, ok := prices[id]
			return price, ok
		}
	}
	tr.apply(1, event.SessionCreated{ID: "t1", Model: sessionModel})
	return tr, sink
}

func costMessage(tr *tracker, modelID string, usage event.Usage) {
	tr.apply(tr.seq+1, event.UserMessage{ItemID: "user", TurnID: "turn-1", Text: "hi", Model: modelID})
	tr.apply(tr.seq+1, event.AssistantMessage{ItemID: "agent", TurnID: "turn-1", StopReason: "stop", Usage: usage})
}

func requireCost(t *testing.T, label string, usage ThreadTokenUsage, micros int64, partial bool) {
	t.Helper()
	if usage.CostUsdMicros == nil {
		t.Fatalf("%s: costUsdMicros is absent, want %d", label, micros)
	}
	if *usage.CostUsdMicros != micros || usage.CostPartial != partial {
		t.Fatalf("%s: cost = %d partial %v, want %d partial %v", label, *usage.CostUsdMicros, usage.CostPartial, micros, partial)
	}
}

func TestUsageCostPartial(t *testing.T) {
	prices := map[string]model.Price{
		"openai/a": {InputMicros: 1_000_000, OutputMicros: 2_000_000},
		"openai/c": {InputMicros: 4_000_000, OutputMicros: 8_000_000},
	}
	tr, sink := costTracker(prices, "openai/a")
	costMessage(tr, "", event.Usage{Input: 1000, Output: 500})
	requireCost(t, "first priced message", sink.usages[0], 2000, false)
	costMessage(tr, "openai/b", event.Usage{Input: 100})
	requireCost(t, "unpriced model after priced", sink.usages[1], 2000, true)
	costMessage(tr, "openai/c", event.Usage{Input: 250, Output: 125})
	requireCost(t, "switched to another priced model", sink.usages[2], 4000, true)
	if sink.usages[0].CostUsdMicros == nil || *sink.usages[0].CostUsdMicros != 2000 {
		t.Fatalf("an earlier notification changed to %v, want 2000", sink.usages[0].CostUsdMicros)
	}

	clean, cleanSink := costTracker(prices, "openai/a")
	costMessage(clean, "", event.Usage{Input: 1000, Output: 500})
	costMessage(clean, "openai/c", event.Usage{Input: 250, Output: 125})
	requireCost(t, "all messages priced", cleanSink.usages[1], 4000, false)
}

func TestUsageCostAbsentUntilAMessageIsPriced(t *testing.T) {
	prices := map[string]model.Price{"openai/a": {InputMicros: 1_000_000, OutputMicros: 2_000_000}}
	tr, sink := costTracker(prices, "openai/b")
	costMessage(tr, "", event.Usage{Input: 100})
	if sink.usages[0].CostUsdMicros != nil || sink.usages[0].CostPartial {
		t.Fatalf("usage with no priced message = %+v, want no cost members", sink.usages[0])
	}
	if sink.usages[0].Last.InputTokens != 100 {
		t.Fatalf("tokens = %d, want 100: an unpriced model must still count tokens", sink.usages[0].Last.InputTokens)
	}
	costMessage(tr, "openai/a", event.Usage{Input: 1000})
	requireCost(t, "priced after unpriced", sink.usages[1], 1000, true)

	bare, bareSink := costTracker(nil, "openai/a")
	costMessage(bare, "", event.Usage{Input: 1000})
	if bareSink.usages[0].CostUsdMicros != nil {
		t.Fatalf("tracker without a price lookup reports cost %d", *bareSink.usages[0].CostUsdMicros)
	}
}

func TestUsageCostCountsCacheTokensAtTheirOwnRates(t *testing.T) {
	prices := map[string]model.Price{"openai/a": {InputMicros: 3_000_000, OutputMicros: 15_000_000, CacheReadMicros: 300_000, CacheWriteMicros: 3_750_000}}
	tr, sink := costTracker(prices, "openai/a")
	costMessage(tr, "", event.Usage{Input: 1000, Output: 500, CacheRead: 2000, CacheWrite: 100})
	requireCost(t, "four token kinds", sink.usages[0], 11475, false)
}

func TestTokenUsageCarriesCostOnEveryWirePath(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	r.cfg.Config.Models = map[string]config.ModelSpec{"openai/gpt-test": {Price: &model.Price{
		InputMicros: 3_000_000, OutputMicros: 15_000_000, CacheReadMicros: 300_000, CacheWriteMicros: 3_750_000,
	}}}
	reopen(t, r)
	r.llm.push(llmReply{body: sseText("hi")})
	first := r.connect()
	threadID := startThread(t, r, first, nil)
	startTurn(t, first, threadID, "hello")

	update := first.await("thread/tokenUsage/updated", nil).Params
	var live ThreadTokenUsageUpdatedNotification
	mustUnmarshal(t, update, &live)
	requireCost(t, "live notification", live.TokenUsage, 165, false)
	var members struct {
		TokenUsage map[string]any `json:"tokenUsage"`
	}
	mustUnmarshal(t, update, &members)
	if _, ok := members.TokenUsage["costPartial"]; ok {
		t.Fatalf("complete cost carries costPartial: %v", members.TokenUsage)
	}
	first.await("turn/completed", nil)

	second := r.connect()
	var resumed ThreadResumeResponse
	second.ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	if resumed.TokenUsage == nil {
		t.Fatal("thread/resume has no tokenUsage member")
	}
	requireCost(t, "thread/resume", *resumed.TokenUsage, 165, false)
	var fork ThreadForkResponse
	second.ok("thread/fork", map[string]any{"threadId": threadID}, &fork)
	if fork.TokenUsage == nil {
		t.Fatal("thread/fork has no tokenUsage member")
	}
	requireCost(t, "thread/fork", *fork.TokenUsage, 165, false)
}

func TestTokenUsageHasNoCostWithoutAPrice(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	r.llm.push(llmReply{body: sseText("hi")})
	first := r.connect()
	threadID := startThread(t, r, first, nil)
	startTurn(t, first, threadID, "hello")

	var live ThreadTokenUsageUpdatedNotification
	mustUnmarshal(t, first.await("thread/tokenUsage/updated", nil).Params, &live)
	if live.TokenUsage.Total.TotalTokens != 27 {
		t.Fatalf("total tokens = %d, want 27", live.TokenUsage.Total.TotalTokens)
	}
	if live.TokenUsage.CostUsdMicros != nil || live.TokenUsage.CostPartial {
		t.Fatalf("model without a price carries cost members: %+v", live.TokenUsage)
	}
	first.await("turn/completed", nil)

	var resumed ThreadResumeResponse
	r.connect().ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	if resumed.TokenUsage == nil || resumed.TokenUsage.CostUsdMicros != nil {
		t.Fatalf("resumed usage = %+v, want usage without cost", resumed.TokenUsage)
	}
}
