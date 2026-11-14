package agent

import (
	"encoding/json/jsontext"
	"errors"
	"math/rand/v2"
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/compact"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/agent/prompt"
	"github.com/gosuda/dalgona/internal/config"
)

const overflowBody = `{"error":{"code":"context_length_exceeded","message":"input is too long"}}`

func textWithInput(body string, input int) response {
	return response{body: strings.Replace(sseText(body), `"input_tokens":20`, `"input_tokens":`+strconv.Itoa(input), 1)}
}

func TestOverflowPrunesCompactsAndRetriesOnce(t *testing.T) {
	t.Parallel()
	h := newHarness(t, func(h *harness) {
		h.cfg.Config.CompactionKeepTokens = 10
		h.cfg.Config.Models = map[string]config.ModelSpec{"openai/gpt-test": {ContextWindow: 100_000_000}}
	})
	big := strings.Repeat(strings.Repeat("x", 59)+"\n", 1000)
	reads := make([]call, 0, 5)
	for i := range 5 {
		name := "f" + strconv.Itoa(i) + ".txt"
		h.write(name, big)
		reads = append(reads, call{id: "r" + strconv.Itoa(i), name: "read", args: `{"path":"` + name + `"}`})
	}
	h.model.push(calls(reads...), text("read all"), failing(400, overflowBody), text("SUMMARY: four files of x"), textWithInput("ok", 20000))
	thread := h.start()
	h.submit(thread, "read the files")
	h.waitIdle(thread)
	h.submit(thread, "next")
	h.waitIdle(thread)
	assertOverflowEvents(t, h.events(thread.ID()))
	if h.model.count() != 5 {
		t.Fatalf("requests = %d", h.model.count())
	}
	retried := userTexts(h.model.request(4))
	if len(retried) == 0 || !strings.Contains(retried[0], "SUMMARY: four files of x") || strings.Contains(h.model.raw(4), big[:1000]) {
		t.Fatalf("retried request texts = %q", retried)
	}
	if err := thread.Compact(t.Context()); err != nil {
		t.Fatalf("manual compaction: %v", err)
	}
	err := thread.Compact(t.Context())
	if !errors.Is(err, ErrNothingToSummarize) || !strings.Contains(h.logs.String(), `msg="compaction: nothing to summarize"`) {
		t.Fatalf("second compaction err = %v, logs = %s", err, h.logs.String())
	}
}

func assertOverflowEvents(t *testing.T, events []event.Seqd) {
	t.Helper()
	var pruned event.ContextPruned
	var marker event.Compaction
	for _, item := range events {
		switch value := item.Event.(type) {
		case event.ContextPruned:
			pruned = value
		case event.Compaction:
			marker = value
		}
	}
	if strings.Join(pruned.CallIDs, ",") != "r0" {
		t.Fatalf("pruned = %+v", pruned)
	}
	if marker.Method != compactOverflow || !strings.Contains(marker.Summary, "SUMMARY: four files of x") {
		t.Fatalf("compaction = %+v", marker)
	}
	if turn := lastTurn(events); turn.Status != statusCompleted {
		t.Fatalf("turn = %+v", turn)
	}
}

func TestSecondOverflowFailsTheTurn(t *testing.T) {
	t.Parallel()
	h := newHarness(t, func(h *harness) { h.cfg.Config.CompactionKeepTokens = 1 })
	h.model.push(text("first"), failing(400, overflowBody), text("summary"), failing(400, overflowBody))
	thread := h.start()
	h.submit(thread, "one")
	h.waitIdle(thread)
	h.submit(thread, "two")
	h.waitIdle(thread)
	if turn := lastTurn(h.events(thread.ID())); turn.Status != statusFailed || turn.Error != "context overflow after compaction" {
		t.Fatalf("turn = %+v", turn)
	}
}

func randomHistory(random *rand.Rand) view {
	var v view
	add := func(e event.Event, size int) {
		v.records = append(v.records, event.Seqd{Seq: uint64(len(v.records) + 1), Event: e})
		v.sizes = append(v.sizes, size)
	}
	add(event.SessionCreated{ID: "s", Cwd: "/w", Model: "m/x", Approval: "auto", Sandbox: "off", Runner: "local"}, 100)
	for turn := range random.IntN(6) + 1 {
		id := "t" + strconv.Itoa(turn)
		add(event.TurnStarted{TurnID: id}, 30)
		add(event.UserMessage{ItemID: "u" + id, TurnID: id, Text: "q", Kind: "prompt"}, random.IntN(4000)+50)
		for step := range random.IntN(4) {
			addRandomStep(random, add, id, step)
		}
		add(event.AssistantMessage{ItemID: "z" + id, TurnID: id, StopReason: "stop"}, random.IntN(3000)+50)
		add(event.TurnCompleted{TurnID: id, Status: "completed"}, 30)
	}
	return v
}

func addRandomStep(random *rand.Rand, add func(event.Event, int), id string, step int) {
	if random.IntN(4) == 0 {
		addBatchedStep(random, add, id, step)
		return
	}
	callID := id + "c" + strconv.Itoa(step)
	message := event.AssistantMessage{
		ItemID: "a" + callID, TurnID: id, StopReason: "tool_calls",
		ToolCalls: []event.ToolCall{{ID: callID, Name: "read", Args: jsontext.Value(`{}`)}},
	}
	add(message, random.IntN(2000)+50)
	add(event.ToolStarted{CallID: callID, TurnID: id, Tool: "read", Class: "read", Args: jsontext.Value(`{}`)}, 80)
	if random.IntN(4) == 0 {
		add(event.UserMessage{ItemID: "s" + callID, TurnID: id, Text: "steer", Kind: "steer"}, random.IntN(200)+10)
	}
	add(event.ToolCompleted{CallID: callID, OK: true, Output: "o", Recovered: "none"}, random.IntN(20000)+50)
}

func addBatchedStep(random *rand.Rand, add func(event.Event, int), id string, step int) {
	callA := id + "c" + strconv.Itoa(step) + "a"
	callB := id + "c" + strconv.Itoa(step) + "b"
	batched := event.AssistantMessage{
		ItemID: "a" + callA, TurnID: id, StopReason: "tool_calls",
		ToolCalls: []event.ToolCall{{ID: callA, Name: "read", Args: jsontext.Value(`{}`)}, {ID: callB, Name: "read", Args: jsontext.Value(`{}`)}},
	}
	add(batched, random.IntN(2000)+50)
	add(event.ToolStarted{CallID: callA, TurnID: id, Tool: "read", Class: "read", Args: jsontext.Value(`{}`)}, 80)
	if random.IntN(2) == 0 {
		add(event.UserMessage{ItemID: "s" + callA, TurnID: id, Text: "steer", Kind: "steer"}, random.IntN(200)+10)
	}
	add(event.ToolCompleted{CallID: callB, Output: "declined", Recovered: "none"}, random.IntN(20000)+50)
	if random.IntN(2) == 0 {
		add(event.UserMessage{ItemID: "s" + callB, TurnID: id, Text: "steer", Kind: "steer"}, random.IntN(200)+10)
	}
	add(event.ToolCompleted{CallID: callA, OK: true, Output: "o", Recovered: "none"}, random.IntN(20000)+50)
}

func TestCompactionCutKeepsCallsWithTheirMessage(t *testing.T) {
	t.Parallel()
	random := rand.New(rand.NewPCG(11, 3))
	thread := &Thread{}
	for iteration := range 500 {
		v := randomHistory(random)
		keep := random.IntN(15000) + 1
		cv := thread.compactionView(v)
		cut, ok := compact.Cut(cv.entries, keep, cv.prevIdx)
		if !ok {
			if cutPossible(cv.entries, keep*4, cv.prevIdx) {
				t.Fatalf("iteration %d: no cut with keep %d tokens", iteration, keep)
			}
			continue
		}
		assertCut(t, iteration, cv, cut, keep)
	}
}

func cutPossible(entries []compact.Entry, target, prev int) bool {
	tail := 0
	for i := len(entries) - 1; i > prev; i-- {
		tail += entries[i].Bytes
		if tail < target {
			continue
		}
		if entries[i].Kind != compact.UserMessage && entries[i].Kind != compact.AssistantMessage {
			continue
		}
		if suffixOrphans(entries, i) {
			continue
		}
		return true
	}
	return false
}

func suffixOrphans(entries []compact.Entry, cut int) bool {
	issued := make(map[string]struct{})
	for i := cut; i < len(entries); i++ {
		for _, id := range entries[i].CallIDs {
			issued[id] = struct{}{}
		}
	}
	for i := cut; i < len(entries); i++ {
		if entries[i].CallID == "" {
			continue
		}
		if entries[i].Kind != compact.ToolStarted && entries[i].Kind != compact.ToolCompleted {
			continue
		}
		if _, ok := issued[entries[i].CallID]; !ok {
			return true
		}
	}
	return false
}

func assertCut(t *testing.T, iteration int, cv compactionView, cut, keep int) {
	t.Helper()
	kept := make(map[string]bool)
	tail := 0
	for i := cut; i < len(cv.events); i++ {
		tail += cv.entries[i].Bytes
		if orphanCall(cv.events[i], kept) {
			t.Fatalf("iteration %d: cut %d separates a call from its assistant message", iteration, cut)
		}
	}
	if tail < keep*4 {
		t.Fatalf("iteration %d: kept tail %d bytes < %d tokens", iteration, tail, keep)
	}
}

func orphanCall(item event.Event, kept map[string]bool) bool {
	switch value := item.(type) {
	case event.AssistantMessage:
		for _, c := range value.ToolCalls {
			kept[c.ID] = true
		}
	case event.ToolStarted:
		return !kept[value.CallID]
	case event.ToolCompleted:
		return !kept[value.CallID]
	}
	return false
}

func TestManualCompactionThatDoesNotShrinkAppendsNoMarker(t *testing.T) {
	t.Parallel()
	h := newHarness(t, func(h *harness) { h.cfg.Config.CompactionKeepTokens = 1 })
	h.model.push(text("first"), text("second"), text(strings.Repeat("s", 40000)))
	thread := h.start()
	h.submit(thread, "one")
	h.waitIdle(thread)
	h.submit(thread, "two")
	h.waitIdle(thread)
	if err := thread.Compact(t.Context()); !errors.Is(err, ErrCompactionNotSmaller) {
		t.Fatalf("manual compaction err = %v", err)
	}
	if markers := compactionMarkers(h.events(thread.ID())); markers != 0 {
		t.Fatalf("compaction markers = %d, want 0", markers)
	}
}

func TestAutoCompactionThatDoesNotShrinkKeepsTheTurn(t *testing.T) {
	t.Parallel()
	h := newHarness(t, func(h *harness) {
		h.cfg.Config.CompactionKeepTokens = 1
		h.cfg.Config.CompactionThreshold = 1e-9
		h.cfg.Config.Models = map[string]config.ModelSpec{"openai/gpt-test": {ContextWindow: 100_000_000}}
	})
	h.model.push(text(strings.Repeat("s", 40000)), text("first"))
	thread := h.start()
	h.submit(thread, "one")
	h.waitIdle(thread)
	events := h.events(thread.ID())
	if turn := lastTurn(events); turn.Status != statusCompleted {
		t.Fatalf("turn = %+v", turn)
	}
	if markers := compactionMarkers(events); markers != 0 {
		t.Fatalf("compaction markers = %d, want 0", markers)
	}
	if h.model.count() != 2 {
		t.Fatalf("requests = %d, want 2", h.model.count())
	}
	if !strings.Contains(h.logs.String(), `msg="compaction: the summary is not smaller than the context it replaces"`) {
		t.Fatalf("logs = %s", h.logs.String())
	}
}

func TestAutoCompactionDoesNotRepeatSummarizer(t *testing.T) {
	t.Parallel()
	h := newHarness(t, func(h *harness) {
		h.cfg.Config.CompactionKeepTokens = 1
		h.cfg.Config.CompactionThreshold = 1e-9
		h.cfg.Config.Models = map[string]config.ModelSpec{"openai/gpt-test": {ContextWindow: 100_000_000}}
	})
	h.write("a.txt", "hello\n")
	h.model.push(
		text(strings.Repeat("s", 40000)),
		calls(call{id: "r1", name: "read", args: `{"path":"a.txt"}`}),
		text(strings.Repeat("s", 40000)),
		text("done"),
	)
	thread := h.start()
	h.submit(thread, "one")
	h.waitIdle(thread)
	events := h.events(thread.ID())
	if turn := lastTurn(events); turn.Status != statusCompleted {
		t.Fatalf("turn = %+v", turn)
	}
	if markers := compactionMarkers(events); markers != 0 {
		t.Fatalf("compaction markers = %d, want 0", markers)
	}
	if got := summarizerRequests(h); got != 1 {
		t.Fatalf("summarizer requests = %d, want 1", got)
	}
	if h.model.count() != 3 {
		t.Fatalf("requests = %d, want 3", h.model.count())
	}
}

func summarizerRequests(h *harness) int {
	n := 0
	for i := range h.model.count() {
		if h.model.request(i)["instructions"] == prompt.Compaction {
			n++
		}
	}
	return n
}

func compactionMarkers(events []event.Seqd) int {
	markers := 0
	for _, item := range events {
		if _, ok := item.Event.(event.Compaction); ok {
			markers++
		}
	}
	return markers
}
