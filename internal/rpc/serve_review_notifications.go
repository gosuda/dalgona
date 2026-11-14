package rpc

import (
	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
)

func (t *tracker) toolJudged(_ uint64, ev event.Event) {
	e, _ := ev.(event.ToolJudged)
	t.sink.notify("dalgona/tool/judged", func() writer {
		return ToolJudgedNotification{
			ThreadID: t.threadID, TurnID: e.TurnID, ItemID: e.CallID, Verdict: e.Verdict,
			Summary: agent.ReviewSummary(agent.ReviewOutcome{Verdict: e.Verdict, Scores: e.Scores, Flags: e.Flags, Spoken: e.Spoken, Error: e.Error, Truncated: e.Truncated}), Paths: e.Paths,
		}
	})
}

func (t *tracker) turnJudged(_ uint64, ev event.Event) {
	e, _ := ev.(event.TurnJudged)
	t.sink.notify("dalgona/turn/judged", func() writer {
		return TurnJudgedNotification{
			ThreadID: t.threadID, TurnID: e.TurnID, Verdict: e.Verdict,
			Summary: agent.ReviewSummary(agent.ReviewOutcome{Verdict: e.Verdict, Scores: e.Scores, Flags: e.Flags, Spoken: e.Spoken, Error: e.Error, Truncated: e.Truncated}), Paths: e.Paths,
		}
	})
}
