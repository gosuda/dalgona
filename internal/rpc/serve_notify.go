package rpc

import "encoding/json/jsontext"

func (n ErrorNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.str("turnId", n.TurnID)
	o.boolean("willRetry", n.WillRetry)
	o.key("error")
	n.Error.write(o)
	o.close()
}

func (n ThreadStartedNotification) write(o *out) {
	o.open()
	o.key("thread")
	n.Thread.write(o)
	o.close()
}

func (n ThreadStatusChangedNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.key("status")
	n.Status.write(o)
	o.key("seq")
	o.token(jsontext.Uint(n.Seq))
	o.close()
}

func (n ThreadNameUpdatedNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.optStr("threadName", n.ThreadName)
	o.close()
}

func (n ThreadTokenUsageUpdatedNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.str("turnId", n.TurnID)
	o.key("tokenUsage")
	n.TokenUsage.write(o)
	o.close()
}

func (u ThreadTokenUsage) write(o *out) {
	o.open()
	o.key("last")
	u.Last.write(o)
	o.key("total")
	u.Total.write(o)
	if u.ModelContextWindow > 0 {
		o.int("modelContextWindow", int64(u.ModelContextWindow))
	}
	o.close()
}

func (b TokenUsageBreakdown) write(o *out) {
	o.open()
	o.int("inputTokens", int64(b.InputTokens))
	o.int("cachedInputTokens", int64(b.CachedInputTokens))
	o.int("cacheWriteInputTokens", int64(b.CacheWriteInputTokens))
	o.int("outputTokens", int64(b.OutputTokens))
	o.int("reasoningOutputTokens", int64(b.ReasoningOutputTokens))
	o.int("totalTokens", int64(b.TotalTokens))
	o.close()
}

func (b TokenUsageBreakdown) add(next TokenUsageBreakdown) TokenUsageBreakdown {
	b.InputTokens += next.InputTokens
	b.CachedInputTokens += next.CachedInputTokens
	b.CacheWriteInputTokens += next.CacheWriteInputTokens
	b.OutputTokens += next.OutputTokens
	b.ReasoningOutputTokens += next.ReasoningOutputTokens
	b.TotalTokens += next.TotalTokens
	return b
}

func (n ThreadRevertedNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.close()
}

func (n TurnStartedNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.key("turn")
	n.Turn.write(o)
	o.close()
}

func (n TurnPlanUpdatedNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.str("turnId", n.TurnID)
	o.key("plan")
	o.openArray()
	for _, step := range n.Plan {
		o.open()
		o.str("step", step.Step)
		o.str("status", step.Status)
		o.close()
	}
	o.closeArray()
	o.close()
}

func (n ItemStartedNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.str("turnId", n.TurnID)
	o.int("startedAtMs", n.StartedAtMs)
	o.key("item")
	n.Item.write(o)
	o.close()
}

func (n ItemCompletedNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.str("turnId", n.TurnID)
	o.int("completedAtMs", n.CompletedAtMs)
	o.key("item")
	n.Item.write(o)
	o.close()
}

func (n AgentMessageDeltaNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.str("turnId", n.TurnID)
	o.str("itemId", n.ItemID)
	o.str("delta", n.Delta)
	o.close()
}

func (n ReasoningTextDeltaNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.str("turnId", n.TurnID)
	o.str("itemId", n.ItemID)
	o.str("delta", n.Delta)
	o.int("contentIndex", int64(n.ContentIndex))
	o.close()
}

func (n ServerRequestResolvedNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.str("requestId", n.RequestID)
	o.close()
}

func (n AccountLoginCompletedNotification) write(o *out) {
	o.open()
	o.optStr("loginId", n.LoginID)
	o.boolean("success", n.Success)
	o.optStr("error", n.Error)
	o.close()
}

func (n ChildSpawnedNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.str("childId", n.ChildID)
	o.str("cwd", n.Cwd)
	o.close()
}

func (n AdvisorNoteNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.str("advisor", n.Advisor)
	o.str("severity", n.Severity)
	o.str("note", n.Note)
	o.str("delivery", n.Delivery)
	o.close()
}

func (p CommandExecutionRequestApprovalParams) write(o *out) {
	o.open()
	o.str("threadId", p.ThreadID)
	o.str("turnId", p.TurnID)
	o.str("itemId", p.ItemID)
	o.int("startedAtMs", p.StartedAtMs)
	o.optStr("command", p.Command)
	o.optStr("cwd", p.Cwd)
	o.optStr("reason", p.Reason)
	o.close()
}

func (p FileChangeRequestApprovalParams) write(o *out) {
	o.open()
	o.str("threadId", p.ThreadID)
	o.str("turnId", p.TurnID)
	o.str("itemId", p.ItemID)
	o.int("startedAtMs", p.StartedAtMs)
	o.optStr("reason", p.Reason)
	o.close()
}

func (n RetryWaitNotification) write(o *out) {
	o.open()
	o.str("threadId", n.ThreadID)
	o.str("turnId", n.TurnID)
	o.key("remainingMs")
	o.token(jsontext.Int(n.RemainingMS))
	o.close()
}
