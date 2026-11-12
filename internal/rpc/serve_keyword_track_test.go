package rpc

import (
	"log/slog"
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestTrackerTakesItsKeywordSourceAtConstruction(t *testing.T) {
	source := func(threadID, text string, kind agent.InputKind) []agent.KeywordMatch {
		if threadID != "t1" || kind != agent.InputPrompt {
			return nil
		}
		return []agent.KeywordMatch{{Word: "probe", Start: 0, End: 5, Summary: "notice added"}}
	}
	tr := newTracker("t1", nil, nil, source, slog.New(slog.DiscardHandler))
	message := event.UserMessage{ItemID: "u1", TurnID: "turn-1", Text: "probe", Notices: []event.Notice{{Word: "probe"}}}
	matches := tr.noticeMatches(message, "probe")
	if len(matches) != 1 || matches[0].Word != "probe" || matches[0].ByteRange.End != 5 {
		t.Fatalf("noticeMatches = %+v, want the probe match from the source given at construction", matches)
	}
}
