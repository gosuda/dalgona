package rpc

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func sseReasoning(parts ...string) string {
	var b strings.Builder
	for _, part := range parts {
		b.WriteString(sseEvent("response.reasoning_summary_text.delta", `{"type":"response.reasoning_summary_text.delta","output_index":0,"delta":`+quoted(part)+`}`))
	}
	return b.String()
}

type reasoningWire struct {
	agentID    string
	text       strings.Builder
	deltaIDs   []string
	firstDelta int
	started    []int
	startedIDs []string
	completed  []string
}

func readReasoning(t *testing.T, notes []message) *reasoningWire {
	t.Helper()
	w := &reasoningWire{firstDelta: -1}
	for i, m := range notes {
		switch m.Method {
		case "item/agentMessage/delta":
			w.agentID = fieldOf(t, m.Params, "itemId")
		case "item/reasoning/textDelta":
			w.deltaIDs = append(w.deltaIDs, fieldOf(t, m.Params, "itemId"))
			w.text.WriteString(fieldOf(t, m.Params, "delta"))
			if w.firstDelta < 0 {
				w.firstDelta = i
			}
		case "item/started":
			if it := itemOf(t, m); it.Type == kindReasoning {
				w.started = append(w.started, i)
				w.startedIDs = append(w.startedIDs, it.ID)
			}
		case "item/completed":
			if it := itemOf(t, m); it.Type == kindReasoning {
				w.completed = append(w.completed, it.ID)
			}
		}
	}
	return w
}

func TestReasoningDeltaCarriesReasoningItemID(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	r.llm.push(llmReply{body: sseReasoning("think ", "harder") + sseText("ans", "wer")})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "ponder")
	w := readReasoning(t, c.collect())

	expect(t, w.agentID != "", "item/agentMessage/delta carries an item id", w.agentID)
	want := w.agentID + "-reasoning"
	expect(t, w.text.String() == "think harder", "item/reasoning/textDelta text", w.text.String())
	for _, id := range w.deltaIDs {
		expect(t, id == want, "item/reasoning/textDelta item id is <item>-reasoning", id)
	}
	expect(t, len(w.started) == 1 && w.startedIDs[0] == want, "item/started once for item <item>-reasoning", w.startedIDs)
	expect(t, w.started[0] < w.firstDelta, "item/started before the first item/reasoning/textDelta", []int{w.started[0], w.firstDelta})
	expect(t, len(w.completed) == 1 && w.completed[0] == want, "item/completed once for item <item>-reasoning", w.completed)
}
