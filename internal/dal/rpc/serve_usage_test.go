package rpc

import (
	json "encoding/json/v2"
	"log/slog"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
)

func TestTokenUsageCarriesContextWindowAndSurvivesResume(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	r.cfg.Config.Models = map[string]config.ModelSpec{"openai/gpt-test": {ContextWindow: 200000}}
	reopen(t, r)
	r.llm.push(llmReply{body: sseText("hi")})
	first := r.connect()
	threadID := startThread(t, r, first, nil)
	startTurn(t, first, threadID, "hello")

	var live struct {
		TokenUsage struct {
			ModelContextWindow int `json:"modelContextWindow"`
		} `json:"tokenUsage"`
	}
	mustUnmarshal(t, first.await("thread/tokenUsage/updated", nil).Params, &live)
	if live.TokenUsage.ModelContextWindow != 200000 {
		t.Fatalf("live modelContextWindow = %d, want 200000", live.TokenUsage.ModelContextWindow)
	}
	first.await("turn/completed", nil)

	second := r.connect()
	var resumed struct {
		TokenUsage *struct {
			Total struct {
				TotalTokens int `json:"totalTokens"`
			} `json:"total"`
			ModelContextWindow int `json:"modelContextWindow"`
		} `json:"tokenUsage"`
	}
	second.ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	if resumed.TokenUsage == nil {
		t.Fatal("thread/resume has no tokenUsage member")
	}
	if resumed.TokenUsage.Total.TotalTokens != 27 {
		t.Fatalf("resumed total tokens = %d, want 27", resumed.TokenUsage.Total.TotalTokens)
	}
	if resumed.TokenUsage.ModelContextWindow != 200000 {
		t.Fatalf("resumed modelContextWindow = %d, want 200000", resumed.TokenUsage.ModelContextWindow)
	}
}

func TestTokenUsageOmittedWithoutUsage(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	r.cfg.Config.Models = map[string]config.ModelSpec{"openai/gpt-test": {ContextWindow: 200000}}
	reopen(t, r)
	first := r.connect()
	threadID := startThread(t, r, first, nil)

	second := r.connect()
	resumed := second.call("thread/resume", map[string]any{"threadId": threadID})
	if resumed.Error != nil {
		t.Fatalf("thread/resume failed: %d %s", resumed.Error.Code, resumed.Error.Message)
	}
	var fields map[string]any
	if err := json.Unmarshal(resumed.Result, &fields); err != nil {
		t.Fatal(err)
	}
	if _, ok := fields["tokenUsage"]; ok {
		t.Fatalf("resumed result %s carries tokenUsage, want it omitted", resumed.Result)
	}
}

type usageSink struct {
	notes []string
	usage []ThreadTokenUsageUpdatedNotification
}

func (s *usageSink) turn(string, Turn) {}

func (s *usageSink) item(string, string, *Item) {}

func (s *usageSink) notify(method string, build func() writer) {
	s.notes = append(s.notes, method)
	if method == "thread/tokenUsage/updated" {
		if n, ok := build().(ThreadTokenUsageUpdatedNotification); ok {
			s.usage = append(s.usage, n)
		}
	}
}

func newUsageTracker() *tracker {
	return newTracker("t1", nil, nil, nil, slog.New(slog.DiscardHandler))
}

func requireNoUsageNotification(t *testing.T, sink *usageSink) {
	t.Helper()
	if len(sink.usage) != 0 {
		t.Fatalf("assistant without usage notified %d times", len(sink.usage))
	}
}

func requireProviderUsageNotification(t *testing.T, sink *usageSink) {
	t.Helper()
	if len(sink.usage) != 1 || sink.usage[0].TokenUsage.EstimatedContextTokens != nil {
		t.Fatalf("provider usage notification = %+v, want no estimate", sink.usage)
	}
}

func requireZeroEstimateNotification(t *testing.T, sink *usageSink) {
	t.Helper()
	if len(sink.usage) != 2 {
		t.Fatalf("compaction notifications = %d, want 2", len(sink.usage))
	}
	zero := sink.usage[1].TokenUsage
	if zero.EstimatedContextTokens == nil || *zero.EstimatedContextTokens != 0 {
		t.Fatalf("genuine zero estimate = %v, want non-nil 0", zero.EstimatedContextTokens)
	}
	want := TokenUsageBreakdown{InputTokens: 7000, OutputTokens: 5, TotalTokens: 7005}
	if zero.Last != want || zero.Total != want {
		t.Fatalf("compaction last/total = %+v/%+v, want %+v", zero.Last, zero.Total, want)
	}
	raw, err := encode(ThreadTokenUsageUpdatedNotification{ThreadID: "t1", TurnID: "turn-1", TokenUsage: zero})
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(raw), `"estimatedContextTokens":0`) {
		t.Fatalf("codec wrote %s, want estimatedContextTokens 0", raw)
	}
}

func requireEstimateOnlySnapshot(t *testing.T) {
	t.Helper()
	fresh := newTracker("t2", nil, nil, nil, slog.New(slog.DiscardHandler))
	fresh.sink = &usageSink{}
	fresh.apply(1, event.Compaction{FirstKeptSeq: 1, Summary: "s", Method: "manual", TokensBefore: 7032, TokensAfter: 2179})
	snap := fresh.usageSnapshot()
	if snap == nil || snap.EstimatedContextTokens == nil || *snap.EstimatedContextTokens != 2179 {
		t.Fatalf("estimate-only snapshot = %+v, want 2179", snap)
	}
}

func requireEstimateRetainedWithoutUsage(t *testing.T, tr *tracker, sink *usageSink) {
	t.Helper()
	tr.apply(5, event.AssistantMessage{TurnID: "turn-1", ItemID: "m3", Text: "quiet"})
	if len(sink.usage) != 2 {
		t.Fatalf("assistant without usage notified %d times, want 2", len(sink.usage))
	}
	if tr.usage.EstimatedContextTokens == nil || *tr.usage.EstimatedContextTokens != 0 {
		t.Fatalf("no-usage assistant cleared the estimate: %v", tr.usage.EstimatedContextTokens)
	}
}

func requireRealUsageClearsEstimate(t *testing.T, tr *tracker, sink *usageSink) {
	t.Helper()
	tr.apply(6, event.AssistantMessage{TurnID: "turn-1", ItemID: "m2", Usage: event.Usage{Input: 20, Output: 7}})
	if len(sink.usage) != 3 {
		t.Fatalf("post-usage notifications = %d, want 3", len(sink.usage))
	}
	if sink.usage[2].TokenUsage.EstimatedContextTokens != nil || tr.usage.EstimatedContextTokens != nil {
		t.Fatalf("real usage did not clear the estimate: notification %+v tracker %+v", sink.usage[2].TokenUsage.EstimatedContextTokens, tr.usage.EstimatedContextTokens)
	}
}

func TestCompactionEstimateLifecycle(t *testing.T) {
	t.Parallel()
	sink := &usageSink{}
	tr := newUsageTracker()
	tr.sink = sink
	tr.apply(1, event.TurnStarted{TurnID: "turn-1"})
	tr.apply(2, event.AssistantMessage{TurnID: "turn-1", ItemID: "m0", Text: "hi"})
	requireNoUsageNotification(t, sink)
	tr.apply(3, event.AssistantMessage{TurnID: "turn-1", ItemID: "m1", Usage: event.Usage{Input: 7000, Output: 5}})
	requireProviderUsageNotification(t, sink)
	tr.apply(4, event.Compaction{FirstKeptSeq: 1, Summary: "s", Method: "manual", TokensBefore: 7032})
	requireZeroEstimateNotification(t, sink)
	requireEstimateOnlySnapshot(t)
	requireEstimateRetainedWithoutUsage(t, tr, sink)
	requireRealUsageClearsEstimate(t, tr, sink)
}

type tokenUsageLast struct {
	InputTokens int `json:"inputTokens"`
	TotalTokens int `json:"totalTokens"`
}

type tokenUsageTotals struct {
	TotalTokens int `json:"totalTokens"`
}

type compactionUsagePayload struct {
	TokenUsage struct {
		Last                   tokenUsageLast   `json:"last"`
		Total                  tokenUsageTotals `json:"total"`
		EstimatedContextTokens *int             `json:"estimatedContextTokens"`
	} `json:"tokenUsage"`
}

type liveCompactionUsage struct {
	TurnID     string `json:"turnId"`
	TokenUsage struct {
		Last                   tokenUsageLast   `json:"last"`
		Total                  tokenUsageTotals `json:"total"`
		EstimatedContextTokens *int             `json:"estimatedContextTokens"`
	} `json:"tokenUsage"`
}

type resumedCompactionUsage struct {
	TokenUsage *struct {
		Last                   tokenUsageLast   `json:"last"`
		Total                  tokenUsageTotals `json:"total"`
		ModelContextWindow     int              `json:"modelContextWindow"`
		EstimatedContextTokens *int             `json:"estimatedContextTokens"`
	} `json:"tokenUsage"`
}

func requireTurnUsageReport(t *testing.T, c *wireClient, wantInput, wantTotal int) {
	t.Helper()
	var first compactionUsagePayload
	mustUnmarshal(t, c.await("thread/tokenUsage/updated", nil).Params, &first)
	if first.TokenUsage.Last.InputTokens != wantInput || first.TokenUsage.Last.TotalTokens != wantTotal {
		t.Fatalf("turn usage = %+v, want %d/%d", first.TokenUsage.Last, wantInput, wantTotal)
	}
}

func requireLiveCompactionEstimate(t *testing.T, c *wireClient, turnID string) {
	t.Helper()
	var live liveCompactionUsage
	c.await("thread/tokenUsage/updated", func(m message) bool {
		mustUnmarshal(t, m.Params, &live)
		return live.TokenUsage.EstimatedContextTokens != nil
	})
	if live.TurnID != turnID {
		t.Fatalf("compaction turnId = %q, want %q", live.TurnID, turnID)
	}
	if live.TokenUsage.EstimatedContextTokens == nil || *live.TokenUsage.EstimatedContextTokens != 2179 {
		t.Fatalf("live estimate = %v, want 2179", live.TokenUsage.EstimatedContextTokens)
	}
	if live.TokenUsage.Last.InputTokens != 7000 || live.TokenUsage.Total.TotalTokens != 7005 {
		t.Fatalf("compaction changed last/total: %+v", live.TokenUsage)
	}
}

func resumedEstimate(t *testing.T, c *wireClient, req map[string]any) *resumedCompactionUsage {
	t.Helper()
	var out resumedCompactionUsage
	c.ok("thread/resume", req, &out)
	if out.TokenUsage == nil || out.TokenUsage.EstimatedContextTokens == nil || *out.TokenUsage.EstimatedContextTokens != 2179 {
		t.Fatalf("resume estimate = %+v, want 2179", out.TokenUsage)
	}
	return &out
}

func requireFullResumeEstimate(t *testing.T, c *wireClient, threadID string) *resumedCompactionUsage {
	t.Helper()
	return resumedEstimate(t, c, map[string]any{"threadId": threadID})
}

func requireReplayResumeEstimate(t *testing.T, c *wireClient, threadID string) {
	t.Helper()
	resumedEstimate(t, c, map[string]any{"threadId": threadID, "excludeTurns": true})
}

func TestCompactionEstimateReplacesStaleUsage(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	r.cfg.Config.Models = map[string]config.ModelSpec{"openai/gpt-test": {ContextWindow: 32000}}
	reopen(t, r)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	reply := sseEvent("response.output_text.delta", `{"type":"response.output_text.delta","output_index":0,"delta":`+quoted("hi")+`}`) +
		sseEvent("response.completed", `{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":7000,"output_tokens":5}}}`)
	r.llm.push(llmReply{body: reply})
	turnID := startTurn(t, c, threadID, "hello")
	requireTurnUsageReport(t, c, 7000, 7005)
	c.await("turn/completed", nil)

	thread, err := r.host.Resume(t.Context(), threadID)
	if err != nil {
		t.Fatal(err)
	}
	if err := thread.Append(t.Context(), event.Compaction{FirstKeptSeq: 14, Summary: "Summary: the user read a.txt and edited notes.md", Method: "manual", TokensBefore: 7032, TokensAfter: 2179}); err != nil {
		t.Fatal(err)
	}
	requireLiveCompactionEstimate(t, c, turnID)

	second := r.connect()
	full := requireFullResumeEstimate(t, second, threadID)
	if full.TokenUsage.Last.InputTokens != 7000 || full.TokenUsage.Total.TotalTokens != 7005 || full.TokenUsage.ModelContextWindow != 32000 {
		t.Fatalf("full resume usage = %+v, want stale last plus window 32000", full.TokenUsage)
	}
	third := r.connect()
	requireReplayResumeEstimate(t, third, threadID)

	requireEstimateClearedByNextUsage(t, r, c, threadID)
}

func requireEstimateClearedByNextUsage(t *testing.T, r *rig, c *wireClient, threadID string) {
	t.Helper()
	r.llm.push(llmReply{body: sseText("again")})
	startTurn(t, c, threadID, "more")
	var cleared struct {
		TokenUsage struct {
			EstimatedContextTokens *int `json:"estimatedContextTokens"`
			Last                   struct {
				InputTokens int `json:"inputTokens"`
			} `json:"last"`
		} `json:"tokenUsage"`
	}
	c.await("thread/tokenUsage/updated", func(m message) bool {
		mustUnmarshal(t, m.Params, &cleared)
		return cleared.TokenUsage.Last.InputTokens == 20
	})
	if cleared.TokenUsage.EstimatedContextTokens != nil {
		t.Fatalf("estimate after next real usage = %v, want absent", *cleared.TokenUsage.EstimatedContextTokens)
	}
}
