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
	"github.com/gosuda/dalgona/internal/config"
)

const overflowBody = `{"error":{"code":"context_length_exceeded","message":"input is too long"}}`

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
	h.model.push(calls(reads...), text("read all"), failing(400, overflowBody), text("SUMMARY: four files of x"), text("ok"))
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
		add(event.AssistantMessage{ItemID: "z" + id, TurnID: id, StopReason: "stop"}, random.IntN(3000)+50)
		add(event.TurnCompleted{TurnID: id, Status: "completed"}, 30)
	}
	return v
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
	depth := batchDepth(entries)
	tail := 0
	for i := len(entries) - 1; i > prev; i-- {
		tail += entries[i].Bytes
		boundary := (entries[i].Kind == compact.UserMessage || entries[i].Kind == compact.AssistantMessage) && depth[i] == 0
		if boundary && tail >= target {
			return true
		}
	}
	return false
}

func batchDepth(entries []compact.Entry) []int {
	depth := make([]int, len(entries)+1)
	open := 0
	for i, entry := range entries {
		if entry.Kind == compact.ToolStarted {
			open++
		} else if entry.Kind == compact.ToolCompleted && open > 0 {
			open--
		}
		depth[i+1] = open
	}
	return depth
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
