package rpcserver

import (
	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func (t *tracker) toolJudged(_ uint64, ev event.Event) {
	e, _ := ev.(event.ToolJudged)
	t.sink.notify("dalgona/tool/judged", func() rpc.Encoder {
		return rpc.ToolJudgedNotification{
			ThreadID: t.threadID, TurnID: e.TurnID, ItemID: e.CallID, Verdict: e.Verdict,
			Summary: agent.ReviewSummary(agent.ReviewOutcome{Verdict: e.Verdict, Scores: e.Scores, Flags: e.Flags, Spoken: e.Spoken, Error: e.Error, Truncated: e.Truncated}), Paths: e.Paths,
			By: e.By, Fallback: e.Fallback, Judges: judgeAttributions(e.Judges),
		}
	})
}

func (t *tracker) turnJudged(_ uint64, ev event.Event) {
	e, _ := ev.(event.TurnJudged)
	t.sink.notify("dalgona/turn/judged", func() rpc.Encoder {
		return rpc.TurnJudgedNotification{
			ThreadID: t.threadID, TurnID: e.TurnID, Verdict: e.Verdict,
			Summary: agent.ReviewSummary(agent.ReviewOutcome{Verdict: e.Verdict, Scores: e.Scores, Flags: e.Flags, Spoken: e.Spoken, Error: e.Error, Truncated: e.Truncated}), Paths: e.Paths,
			By: e.By, Fallback: e.Fallback, Judges: judgeAttributions(e.Judges),
		}
	})
}

func judgeAttributions(judges []event.JudgeAttribution) []rpc.JudgeAttribution {
	out := make([]rpc.JudgeAttribution, len(judges))
	for i, judge := range judges {
		out[i] = rpc.JudgeAttribution(judge)
	}
	return out
}
