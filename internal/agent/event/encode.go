package event

import (
	"encoding/json/jsontext"
	"fmt"
	"maps"
	"slices"
	"time"
)

func eventMembers(e Event) ([]member, error) {
	switch e.Type() {
	case TypeSessionCreated, TypeRunnerBound, TypeTurnStarted, TypeTurnCompleted, TypeUserMessage:
		return sessionMembers(e)
	case TypeAssistantMessage, TypeToolStarted, TypeToolCompleted, TypeModelRetry, TypeApprovalRequested, TypeApprovalResolved:
		return toolMembers(e)
	case TypeCompaction, TypeContextPruned, TypeChildSpawned, TypeBranch:
		return stateMembers(e)
	case TypeScriptStarted, TypeScriptEffect, TypeScriptCompleted:
		return scriptMembers(e)
	case TypeAdvisorNote, TypeThreadNamed, TypeThreadArchived:
		return metadataMembers(e)
	default:
		return nil, fmt.Errorf("event: unknown event type %q", e.Type())
	}
}

func sessionMembers(e Event) ([]member, error) {
	switch value := e.(type) {
	case SessionCreated:
		return sessionCreatedMembers(value), nil
	case RunnerBound:
		return []member{
			field("runner", stringValue(value.Runner)), field("service", stringValue(value.Service)),
			field("box_id", stringValue(value.BoxID)), field("baseline", stringValue(value.Baseline)),
		}, nil
	case TurnStarted:
		return []member{field("turn_id", stringValue(value.TurnID))}, nil
	case TurnCompleted:
		fields := []member{field("turn_id", stringValue(value.TurnID)), field("status", stringValue(value.Status))}
		return optionalString(fields, "error", value.Error), nil
	case UserMessage:
		return userMessageMembers(value), nil
	default:
		return nil, fmt.Errorf("event: %T does not match type %s", e, e.Type())
	}
}

func sessionCreatedMembers(e SessionCreated) []member {
	fields := []member{
		field("id", stringValue(e.ID)), field("cwd", stringValue(e.Cwd)),
		field("model", stringValue(e.Model)), field("approval", stringValue(e.Approval)),
		field("sandbox", stringValue(e.Sandbox)), field("runner", stringValue(e.Runner)),
	}
	fields = optionalString(fields, "parent", e.Parent)
	fields = optionalString(fields, "fork_of", e.ForkOf)
	fields = optionalString(fields, "effort", e.Effort)
	fields = optionalString(fields, "advisor", e.Advisor)
	if !e.CreatedAt.IsZero() {
		fields = append(fields, field("created_at", stringValue(e.CreatedAt.UTC().Format(time.RFC3339Nano))))
	}
	if e.InstructionsBlob != nil {
		fields = append(fields, field("instructions_blob", blobValue(e.InstructionsBlob)))
	}
	return fields
}

func advisorNoteMembers(e Event) ([]member, error) {
	value, ok := e.(AdvisorNote)
	if !ok {
		return nil, fmt.Errorf("event: %T does not match type %s", e, e.Type())
	}
	return []member{
		field("advisor", stringValue(value.Advisor)), field("severity", stringValue(value.Severity)),
		field("note", stringValue(value.Note)), field("review_seq", uint64Value(value.ReviewSeq)),
		field("delivery", stringValue(value.Delivery)),
	}, nil
}

func metadataMembers(e Event) ([]member, error) {
	switch value := e.(type) {
	case AdvisorNote:
		return advisorNoteMembers(value)
	case ThreadNamed:
		return []member{field("name", stringValue(value.Name))}, nil
	case ThreadArchived:
		return []member{field("archived", boolValue(value.Archived))}, nil
	default:
		return nil, fmt.Errorf("event: %T does not match type %s", e, e.Type())
	}
}

func userMessageMembers(e UserMessage) []member {
	fields := []member{
		field("item_id", stringValue(e.ItemID)), field("turn_id", stringValue(e.TurnID)),
		field("text", stringValue(e.Text)), field("kind", stringValue(e.Kind)),
	}
	fields = optionalBlobField(fields, "text_blob", e.TextBlob)
	if len(e.Notices) > 0 {
		fields = append(fields, field("notices", arrayValue(e.Notices, noticeValue)))
	}
	if len(e.Mentions) > 0 {
		fields = append(fields, field("mentions", arrayValue(e.Mentions, mentionValue)))
	}
	if e.Verbatim {
		fields = append(fields, field("verbatim", boolValue(true)))
	}
	fields = optionalString(fields, "model", e.Model)
	fields = optionalString(fields, "approval", e.Approval)
	fields = optionalString(fields, "sandbox", e.Sandbox)
	return optionalString(fields, "effort", e.Effort)
}

func noticeValue(e Notice) jsontext.Value {
	fields := []member{field("word", stringValue(e.Word)), field("text", stringValue(e.Text))}
	return objectValue(optionalBlobField(fields, "text_blob", e.TextBlob)...)
}

func mentionValue(e Mention) jsontext.Value {
	fields := []member{field("path", stringValue(e.Path)), field("text", stringValue(e.Text))}
	return objectValue(optionalBlobField(fields, "text_blob", e.TextBlob)...)
}

func toolMembers(e Event) ([]member, error) {
	switch value := e.(type) {
	case AssistantMessage:
		return assistantMessageMembers(value), nil
	case ToolStarted:
		return toolStartedMembers(value), nil
	case ToolCompleted:
		return toolCompletedMembers(value), nil
	case ModelRetry:
		return modelRetryMembers(value), nil
	case ApprovalRequested:
		fields := []member{
			field("request_id", stringValue(value.RequestID)), field("call_id", stringValue(value.CallID)),
			field("summary", stringValue(value.Summary)),
		}
		return optionalString(optionalString(fields, "class", value.Class), "grant_prefix", value.GrantPrefix), nil
	case ApprovalResolved:
		return approvalResolvedMembers(value), nil
	default:
		return nil, fmt.Errorf("event: %T does not match type %s", e, e.Type())
	}
}

func assistantMessageMembers(e AssistantMessage) []member {
	fields := []member{
		field("item_id", stringValue(e.ItemID)), field("turn_id", stringValue(e.TurnID)),
		field("text", stringValue(e.Text)), field("reasoning", stringValue(e.Reasoning)),
		field("tool_calls", arrayValue(e.ToolCalls, toolCallValue)), field("usage", usageValue(e.Usage)),
		field("stop_reason", stringValue(e.StopReason)),
	}
	fields = optionalBlobField(fields, "text_blob", e.TextBlob)
	fields = optionalBlobField(fields, "reasoning_blob", e.ReasoningBlob)
	if e.ContextSeq != 0 {
		fields = append(fields, field("context_seq", uint64Value(e.ContextSeq)))
	}
	return fields
}

func toolCallValue(e ToolCall) jsontext.Value {
	fields := []member{
		field("id", stringValue(e.ID)), field("name", stringValue(e.Name)),
		field("freeform", boolValue(e.Freeform)), field("resolved", boolValue(e.Resolved)),
	}
	fields = optionalRaw(fields, "args", e.Args)
	fields = optionalBlobField(fields, "args_blob", e.ArgsBlob)
	fields = optionalString(fields, "signature", e.Signature)
	if len(e.RedactedThinking) > 0 {
		fields = append(fields, field("redacted_thinking", arrayValue(e.RedactedThinking, stringValue)))
	}
	return objectValue(fields...)
}

func usageValue(e Usage) jsontext.Value {
	return objectValue(
		field("input", intValue(e.Input)), field("output", intValue(e.Output)),
		field("cache_read", intValue(e.CacheRead)), field("cache_write", intValue(e.CacheWrite)),
	)
}

func toolStartedMembers(e ToolStarted) []member {
	fields := []member{
		field("call_id", stringValue(e.CallID)), field("turn_id", stringValue(e.TurnID)),
		field("tool", stringValue(e.Tool)), field("class", stringValue(e.Class)),
		field("repairs", intValue(e.Repairs)),
	}
	fields = optionalRaw(fields, "args", e.Args)
	fields = optionalBlobField(fields, "args_blob", e.ArgsBlob)
	if len(e.Files) > 0 {
		fields = append(fields, field("files", arrayValue(e.Files, fileChangeValue)))
	}
	return fields
}

func fileChangeValue(e FileChange) jsontext.Value {
	fields := []member{
		field("path", stringValue(e.Path)), field("pre_sha", stringValue(e.PreSHA)),
		field("post_sha", stringValue(e.PostSHA)),
	}
	if e.PreBlob != nil {
		fields = append(fields, field("pre_blob", blobValue(e.PreBlob)))
	}
	return objectValue(fields...)
}

func toolCompletedMembers(e ToolCompleted) []member {
	fields := []member{
		field("call_id", stringValue(e.CallID)), field("ok", boolValue(e.OK)),
		field("output", stringValue(e.Output)), field("recovered", stringValue(e.Recovered)),
	}
	if e.Spill != nil {
		fields = append(fields, field("spill", spillValue(*e.Spill)))
	}
	if e.ExitCode != nil {
		fields = append(fields, field("exit_code", intValue(*e.ExitCode)))
	}
	return optionalRaw(fields, "meta", e.Meta)
}

func spillValue(e Spill) jsontext.Value {
	return objectValue(
		field("path", stringValue(e.Path)), field("bytes", int64Value(e.Bytes)),
		field("sha256", stringValue(e.SHA256)),
	)
}

func modelRetryMembers(e ModelRetry) []member {
	return []member{
		field("turn_id", stringValue(e.TurnID)), field("attempt", intValue(e.Attempt)),
		field("verdict", stringValue(e.Verdict)), field("status", intValue(e.Status)),
		field("delay_ms", int64Value(e.DelayMS)),
	}
}

func approvalResolvedMembers(e ApprovalResolved) []member {
	fields := []member{
		field("request_id", stringValue(e.RequestID)), field("decision", stringValue(e.Decision)),
		field("by", stringValue(e.By)), field("reason", stringValue(e.Reason)),
	}
	if len(e.Scores) > 0 {
		fields = append(fields, field("scores", scoresValue(e.Scores)))
	}
	return fields
}

func scoresValue(scores map[string]float64) jsontext.Value {
	keys := slices.Sorted(maps.Keys(scores))
	fields := make([]member, 0, len(keys))
	for _, key := range keys {
		fields = append(fields, field(key, floatValue(scores[key])))
	}
	return objectValue(fields...)
}

func stateMembers(e Event) ([]member, error) {
	switch value := e.(type) {
	case Compaction:
		return compactionMembers(value), nil
	case ContextPruned:
		return []member{
			field("turn_id", stringValue(value.TurnID)),
			field("call_ids", arrayValue(value.CallIDs, stringValue)),
		}, nil
	case ChildSpawned:
		return []member{
			field("call_id", stringValue(value.CallID)), field("child_id", stringValue(value.ChildID)),
			field("runner", stringValue(value.Runner)),
		}, nil
	case Branch:
		return []member{field("parent_seq", uint64Value(value.ParentSeq))}, nil
	default:
		return nil, fmt.Errorf("event: %T does not match type %s", e, e.Type())
	}
}

func compactionMembers(e Compaction) []member {
	return []member{
		field("first_kept_seq", uint64Value(e.FirstKeptSeq)), field("summary", stringValue(e.Summary)),
		field("method", stringValue(e.Method)), field("tokens_before", intValue(e.TokensBefore)),
		field("tokens_after", intValue(e.TokensAfter)),
	}
}

func scriptMembers(e Event) ([]member, error) {
	switch value := e.(type) {
	case ScriptStarted:
		fields := []member{
			field("run_id", stringValue(value.RunID)), field("source_blob", blobValue(&value.SourceBlob)),
			field("entry", stringValue(value.Entry)), field("seed", uint64Value(value.Seed)),
		}
		fields = optionalRaw(fields, "args", value.Args)
		return optionalBlobField(fields, "args_blob", value.ArgsBlob), nil
	case ScriptEffect:
		fields := []member{
			field("run_id", stringValue(value.RunID)), field("n", uint64Value(value.N)),
			field("op", stringValue(value.Op)), field("args_sha", stringValue(value.ArgsSHA)),
		}
		fields = optionalRaw(fields, "result", value.Result)
		fields = optionalBlobField(fields, "result_blob", value.ResultBlob)
		fields = optionalString(fields, "error", value.Error)
		if value.Denied {
			fields = append(fields, field("denied", boolValue(true)))
		}
		return fields, nil
	case ScriptCompleted:
		fields := []member{field("run_id", stringValue(value.RunID)), field("ok", boolValue(value.OK))}
		fields = optionalRaw(fields, "value", value.Value)
		fields = optionalBlobField(fields, "value_blob", value.ValueBlob)
		return optionalString(fields, "error", value.Error), nil
	default:
		return nil, fmt.Errorf("event: %T does not match type %s", e, e.Type())
	}
}

func optionalBlobField(fields []member, name string, blob *BlobRef) []member {
	if blob == nil {
		return fields
	}
	return append(fields, field(name, blobValue(blob)))
}
