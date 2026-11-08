package event_test

import (
	"encoding/json/jsontext"
	"math/rand/v2"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestValidateRejectsInvalidEvents(t *testing.T) {
	t.Parallel()
	events := randomEvents(rand.New(rand.NewPCG(7, 9)))
	session := as[event.SessionCreated](t, events[0])
	runner := as[event.RunnerBound](t, events[1])
	turnStarted := as[event.TurnStarted](t, events[2])
	turnCompleted := as[event.TurnCompleted](t, events[3])
	user := as[event.UserMessage](t, events[4])
	assistant := as[event.AssistantMessage](t, events[5])
	toolStarted := as[event.ToolStarted](t, events[6])
	toolCompleted := as[event.ToolCompleted](t, events[7])
	retry := as[event.ModelRetry](t, events[8])
	approvalRequest := as[event.ApprovalRequested](t, events[9])
	approvalResolved := as[event.ApprovalResolved](t, events[10])
	compaction := as[event.Compaction](t, events[11])
	pruned := as[event.ContextPruned](t, events[12])
	child := as[event.ChildSpawned](t, events[13])
	scriptStarted := as[event.ScriptStarted](t, events[15])
	scriptEffect := as[event.ScriptEffect](t, events[16])
	scriptCompleted := as[event.ScriptCompleted](t, events[17])
	invalid := []event.Event{
		changed(session, func(e *event.SessionCreated) { e.ID = "" }),
		changed(session, func(e *event.SessionCreated) { e.Cwd = "" }),
		changed(session, func(e *event.SessionCreated) { e.Model = "" }),
		changed(session, func(e *event.SessionCreated) { e.Runner = "" }),
		changed(session, func(e *event.SessionCreated) { e.Approval = "sometimes" }),
		changed(session, func(e *event.SessionCreated) { e.Sandbox = "everything" }),
		changed(session, func(e *event.SessionCreated) { e.InstructionsBlob = &event.BlobRef{} }),
		changed(runner, func(e *event.RunnerBound) { e.Runner = "" }),
		changed(runner, func(e *event.RunnerBound) { e.Service = "" }),
		changed(runner, func(e *event.RunnerBound) { e.BoxID = "" }),
		changed(runner, func(e *event.RunnerBound) { e.Baseline = "" }),
		changed(turnStarted, func(e *event.TurnStarted) { e.TurnID = "" }),
		changed(turnCompleted, func(e *event.TurnCompleted) { e.TurnID = "" }),
		changed(turnCompleted, func(e *event.TurnCompleted) { e.Status = "running" }),
		changed(user, func(e *event.UserMessage) { e.ItemID = "" }),
		changed(user, func(e *event.UserMessage) { e.TurnID = "" }),
		changed(user, func(e *event.UserMessage) { e.Kind = "interrupt" }),
		changed(user, func(e *event.UserMessage) {
			e.Notices = slices.Clone(e.Notices)
			e.Notices[0].Word = ""
		}),
		changed(user, func(e *event.UserMessage) {
			e.Notices = slices.Clone(e.Notices)
			e.Notices[0].Text = ""
		}),
		changed(user, func(e *event.UserMessage) {
			e.Mentions = slices.Clone(e.Mentions)
			e.Mentions[0].Path = ""
		}),
		changed(assistant, func(e *event.AssistantMessage) { e.ItemID = "" }),
		changed(assistant, func(e *event.AssistantMessage) { e.TurnID = "" }),
		changed(assistant, func(e *event.AssistantMessage) { e.StopReason = "" }),
		changed(assistant, func(e *event.AssistantMessage) {
			e.ToolCalls = slices.Clone(e.ToolCalls)
			e.ToolCalls[0].ID = ""
		}),
		changed(assistant, func(e *event.AssistantMessage) {
			e.ToolCalls = slices.Clone(e.ToolCalls)
			e.ToolCalls[0].Name = ""
		}),
		changed(assistant, func(e *event.AssistantMessage) {
			e.ToolCalls = slices.Clone(e.ToolCalls)
			e.ToolCalls[0].Args = nil
		}),
		changed(assistant, func(e *event.AssistantMessage) { e.Usage.Input = -1 }),
		changed(toolStarted, func(e *event.ToolStarted) { e.CallID = "" }),
		changed(toolStarted, func(e *event.ToolStarted) { e.TurnID = "" }),
		changed(toolStarted, func(e *event.ToolStarted) { e.Tool = "" }),
		changed(toolStarted, func(e *event.ToolStarted) { e.Class = "network" }),
		changed(toolStarted, func(e *event.ToolStarted) { e.Args = jsontext.Value(`{`) }),
		changed(toolStarted, func(e *event.ToolStarted) { e.Repairs = -1 }),
		changed(toolStarted, func(e *event.ToolStarted) {
			e.Files = slices.Clone(e.Files)
			e.Files[0].Path = ""
		}),
		changed(toolStarted, func(e *event.ToolStarted) {
			e.Files = slices.Clone(e.Files)
			e.Files[0].PreSHA = "bad"
		}),
		changed(toolStarted, func(e *event.ToolStarted) {
			e.Files = slices.Clone(e.Files)
			e.Files[0].PreBlob = &event.BlobRef{}
		}),
		changed(toolCompleted, func(e *event.ToolCompleted) { e.CallID = "" }),
		changed(toolCompleted, func(e *event.ToolCompleted) { e.Recovered = "rerun" }),
		changed(toolCompleted, func(e *event.ToolCompleted) { e.Spill = &event.Spill{} }),
		changed(toolCompleted, func(e *event.ToolCompleted) { e.Meta = jsontext.Value(`{`) }),
		changed(retry, func(e *event.ModelRetry) { e.TurnID = "" }),
		changed(retry, func(e *event.ModelRetry) { e.Attempt = 0 }),
		changed(retry, func(e *event.ModelRetry) { e.Verdict = "terminal" }),
		changed(retry, func(e *event.ModelRetry) { e.DelayMS = -1 }),
		changed(approvalRequest, func(e *event.ApprovalRequested) { e.RequestID = "" }),
		changed(approvalRequest, func(e *event.ApprovalRequested) { e.CallID = "" }),
		changed(approvalRequest, func(e *event.ApprovalRequested) { e.Summary = "" }),
		changed(approvalResolved, func(e *event.ApprovalResolved) { e.RequestID = "" }),
		changed(approvalResolved, func(e *event.ApprovalResolved) { e.Decision = "later" }),
		changed(approvalResolved, func(e *event.ApprovalResolved) { e.By = "" }),
		changed(approvalResolved, func(e *event.ApprovalResolved) { e.Scores = map[string]float64{"x": 2} }),
		changed(compaction, func(e *event.Compaction) { e.FirstKeptSeq = 0 }),
		changed(compaction, func(e *event.Compaction) { e.Summary = "" }),
		changed(compaction, func(e *event.Compaction) { e.Method = "forced" }),
		changed(compaction, func(e *event.Compaction) { e.TokensBefore = -1 }),
		changed(pruned, func(e *event.ContextPruned) { e.TurnID = "" }),
		changed(pruned, func(e *event.ContextPruned) { e.CallIDs = nil }),
		changed(pruned, func(e *event.ContextPruned) {
			e.CallIDs = slices.Clone(e.CallIDs)
			e.CallIDs[0] = ""
		}),
		changed(child, func(e *event.ChildSpawned) { e.CallID = "" }),
		changed(child, func(e *event.ChildSpawned) { e.ChildID = "" }),
		changed(child, func(e *event.ChildSpawned) { e.Runner = "" }),
		changed(scriptStarted, func(e *event.ScriptStarted) { e.RunID = "" }),
		changed(scriptStarted, func(e *event.ScriptStarted) { e.Entry = "" }),
		changed(scriptStarted, func(e *event.ScriptStarted) { e.SourceBlob = event.BlobRef{} }),
		changed(scriptStarted, func(e *event.ScriptStarted) { e.Args = nil }),
		changed(scriptEffect, func(e *event.ScriptEffect) { e.RunID = "" }),
		changed(scriptEffect, func(e *event.ScriptEffect) { e.Op = "" }),
		changed(scriptEffect, func(e *event.ScriptEffect) { e.ArgsSHA = "bad" }),
		changed(scriptEffect, func(e *event.ScriptEffect) { e.Result = nil }),
		changed(scriptCompleted, func(e *event.ScriptCompleted) { e.RunID = "" }),
		event.ScriptCompleted{RunID: "run", OK: true},
		event.ScriptCompleted{RunID: "run", OK: true, Value: jsontext.Value(`null`), Error: "bad"},
		event.ScriptCompleted{RunID: "run"},
		event.ScriptCompleted{RunID: "run", Value: jsontext.Value(`null`), Error: "bad"},
	}
	for i, candidate := range invalid {
		if err := candidate.Validate(); err == nil {
			t.Errorf("invalid event %d (%s) passed validation: %#v", i, candidate.Type(), candidate)
		}
	}
}

func TestValidateRejectsInlinePayloadsOverLimit(t *testing.T) {
	t.Parallel()
	large := strings.Repeat("x", event.MaxInlineBytes+1)
	candidates := []event.Event{
		event.UserMessage{ItemID: "i", TurnID: "t", Text: large, Kind: "prompt"},
		event.ToolCompleted{CallID: "c", Output: large, Recovered: "none"},
		event.ToolStarted{CallID: "c", TurnID: "t", Tool: "read", Class: "read", Args: jsontext.Value(large)},
	}
	for _, candidate := range candidates {
		if err := candidate.Validate(); err == nil {
			t.Errorf("%s accepted an oversized inline payload", candidate.Type())
		}
	}
}

func changed[T any](value T, change func(*T)) T {
	change(&value)
	return value
}

func as[T any](t *testing.T, value any) T {
	t.Helper()
	converted, ok := value.(T)
	if !ok {
		t.Fatalf("value has type %T, want %T", value, *new(T))
	}
	return converted
}
