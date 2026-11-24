package rpc

import "encoding/json/jsontext"

func (n ErrorNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("turnId", n.TurnID)
	o.Bool("willRetry", n.WillRetry)
	o.Key("error")
	n.Error.Encode(o)
	o.Close()
}

func (n ThreadStartedNotification) Encode(o *Writer) {
	o.Open()
	o.Key("thread")
	n.Thread.Encode(o)
	o.Close()
}

func (n ThreadStatusChangedNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Key("status")
	n.Status.Encode(o)
	o.Key("seq")
	o.Token(jsontext.Uint(n.Seq))
	o.Close()
}

func (n ThreadNameUpdatedNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.OptStr("threadName", n.ThreadName)
	o.Close()
}

func (n ThreadTokenUsageUpdatedNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("turnId", n.TurnID)
	o.Key("tokenUsage")
	n.TokenUsage.Encode(o)
	o.Close()
}

func (u ThreadTokenUsage) Encode(o *Writer) {
	o.Open()
	o.Key("last")
	u.Last.Encode(o)
	o.Key("total")
	u.Total.Encode(o)
	if u.ModelContextWindow > 0 {
		o.Int("modelContextWindow", int64(u.ModelContextWindow))
	}
	if u.EstimatedContextTokens != nil {
		o.Int("estimatedContextTokens", int64(*u.EstimatedContextTokens))
	}
	if u.CostUsdMicros != nil {
		o.Int("costUsdMicros", *u.CostUsdMicros)
	}
	if u.CostPartial {
		o.Bool("costPartial", true)
	}
	o.Close()
}

func (b TokenUsageBreakdown) Encode(o *Writer) {
	o.Open()
	o.Int("inputTokens", int64(b.InputTokens))
	o.Int("cachedInputTokens", int64(b.CachedInputTokens))
	o.Int("cacheWriteInputTokens", int64(b.CacheWriteInputTokens))
	o.Int("outputTokens", int64(b.OutputTokens))
	o.Int("reasoningOutputTokens", int64(b.ReasoningOutputTokens))
	o.Int("totalTokens", int64(b.TotalTokens))
	o.Close()
}

func (b TokenUsageBreakdown) Add(next TokenUsageBreakdown) TokenUsageBreakdown {
	b.InputTokens += next.InputTokens
	b.CachedInputTokens += next.CachedInputTokens
	b.CacheWriteInputTokens += next.CacheWriteInputTokens
	b.OutputTokens += next.OutputTokens
	b.ReasoningOutputTokens += next.ReasoningOutputTokens
	b.TotalTokens += next.TotalTokens
	return b
}

func (n ThreadRevertedNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Close()
}

func (n TurnStartedNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Key("turn")
	n.Turn.Encode(o)
	o.Close()
}

func (n TurnPlanUpdatedNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("turnId", n.TurnID)
	o.Key("plan")
	o.OpenArray()
	for _, step := range n.Plan {
		o.Open()
		o.Str("step", step.Step)
		o.Str("status", step.Status)
		o.Close()
	}
	o.CloseArray()
	o.Close()
}

func (n ItemStartedNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("turnId", n.TurnID)
	o.Int("startedAtMs", n.StartedAtMs)
	o.Key("item")
	n.Item.Encode(o)
	o.Close()
}

func (n ItemCompletedNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("turnId", n.TurnID)
	o.Int("completedAtMs", n.CompletedAtMs)
	o.Key("item")
	n.Item.Encode(o)
	o.Close()
}

func (n AgentMessageDeltaNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("turnId", n.TurnID)
	o.Str("itemId", n.ItemID)
	o.Str("delta", n.Delta)
	o.Close()
}

func (n ReasoningTextDeltaNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("turnId", n.TurnID)
	o.Str("itemId", n.ItemID)
	o.Str("delta", n.Delta)
	o.Int("contentIndex", int64(n.ContentIndex))
	o.Close()
}

func (n ServerRequestResolvedNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("requestId", n.RequestID)
	o.OptStr("resolution", n.Resolution)
	o.Close()
}

func (p ToolRequestUserInputParams) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", p.ThreadID)
	o.Str("turnId", p.TurnID)
	o.Str("itemId", p.ItemID)
	o.Bool("isBlocking", p.IsBlocking)
	o.Key("questions")
	o.OpenArray()
	for _, question := range p.Questions {
		question.Encode(o)
	}
	o.CloseArray()
	o.Close()
}

func (q ToolRequestUserInputQuestion) Encode(o *Writer) {
	o.Open()
	o.Str("id", q.ID)
	o.Str("header", q.Header)
	o.Str("question", q.Question)
	o.Bool("isOther", q.IsOther)
	o.Bool("isSecret", q.IsSecret)
	o.Bool("multiSelect", q.MultiSelect)
	o.Key("options")
	if q.Options == nil {
		o.Null()
	} else {
		o.OpenArray()
		for _, option := range q.Options {
			option.Encode(o)
		}
		o.CloseArray()
	}
	o.Close()
}

func (p ToolRequestUserInputOption) Encode(o *Writer) {
	o.Open()
	o.Str("label", p.Label)
	o.Str("description", p.Description)
	o.OptStr("preview", p.Preview)
	o.Close()
}

func (n AccountLoginCompletedNotification) Encode(o *Writer) {
	o.Open()
	o.OptStr("loginId", n.LoginID)
	o.Bool("success", n.Success)
	o.OptStr("error", n.Error)
	o.OptStr("provider", n.Provider)
	o.Close()
}

func (n ChildSpawnedNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("childId", n.ChildID)
	o.Str("cwd", n.Cwd)
	o.Close()
}

func (n AdvisorNoteNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("advisor", n.Advisor)
	o.Str("severity", n.Severity)
	o.Str("note", n.Note)
	o.Str("delivery", n.Delivery)
	o.Close()
}

func (n ToolJudgedNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("turnId", n.TurnID)
	o.Str("itemId", n.ItemID)
	o.Str("verdict", n.Verdict)
	o.Str("summary", n.Summary)
	writePaths(o, n.Paths)
	writeAttribution(o, n.By, n.Fallback, n.Judges)
	o.Close()
}

func (n TurnJudgedNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("turnId", n.TurnID)
	o.Str("verdict", n.Verdict)
	o.Str("summary", n.Summary)
	writePaths(o, n.Paths)
	writeAttribution(o, n.By, n.Fallback, n.Judges)
	o.Close()
}

func (n TurnEffortNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("turnId", n.TurnID)
	o.Str("effort", n.Effort)
	o.Close()
}

func writePaths(o *Writer, paths []string) {
	o.Key("paths")
	o.OpenArray()
	for _, path := range paths {
		o.Text(path)
	}
	o.CloseArray()
}

func writeAttribution(o *Writer, by, fallback string, judges []JudgeAttribution) {
	o.Str("by", by)
	o.OptStr("fallback", fallback)
	if len(judges) == 0 {
		return
	}
	o.Key("judges")
	o.OpenArray()
	for _, judge := range judges {
		o.Open()
		o.Int("part", int64(judge.Part))
		o.Str("by", judge.By)
		o.OptStr("fallback", judge.Fallback)
		o.Close()
	}
	o.CloseArray()
}

func (p CommandExecutionRequestApprovalParams) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", p.ThreadID)
	o.Str("turnId", p.TurnID)
	o.Str("itemId", p.ItemID)
	o.Int("startedAtMs", p.StartedAtMs)
	o.OptStr("command", p.Command)
	o.OptStr("cwd", p.Cwd)
	o.OptStr("reason", p.Reason)
	if p.Escalated {
		o.Bool("escalated", true)
	}
	o.Close()
}

func (p FileChangeRequestApprovalParams) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", p.ThreadID)
	o.Str("turnId", p.TurnID)
	o.Str("itemId", p.ItemID)
	o.Int("startedAtMs", p.StartedAtMs)
	o.OptStr("reason", p.Reason)
	o.Close()
}

func (n FileChangePatchUpdatedNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("turnId", n.TurnID)
	o.Str("itemId", n.ItemID)
	o.Key("changes")
	o.OpenArray()
	for _, change := range n.Changes {
		change.Encode(o)
	}
	o.CloseArray()
	o.Close()
}

func (n RetryWaitNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("turnId", n.TurnID)
	o.Key("remainingMs")
	o.Token(jsontext.Int(n.RemainingMS))
	o.Close()
}

func (n WarningNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("message", n.Message)
	o.Close()
}

func (n SandboxChosenNotification) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", n.ThreadID)
	o.Str("cause", n.Cause)
	o.Str("choice", n.Choice)
	o.Close()
}

func (f SandboxFallback) Encode(o *Writer) {
	o.Open()
	o.Str("cause", f.Cause)
	o.Key("choice")
	if f.Choice == nil {
		o.Null()
	} else {
		o.Text(*f.Choice)
	}
	o.Close()
}
