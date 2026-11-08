package event

import (
	"encoding/json/jsontext"
	"fmt"
)

func decodeSessionEvent(kind string, r *objectReader) (Event, error) {
	switch kind {
	case TypeSessionCreated:
		return SessionCreated{
			ID: r.str("id"), Parent: r.str("parent"), ForkOf: r.str("fork_of"), Cwd: r.str("cwd"),
			Model: r.str("model"), Approval: r.str("approval"), Sandbox: r.str("sandbox"), Effort: r.str("effort"),
			Runner: r.str("runner"), InstructionsBlob: r.blob("instructions_blob"), Advisor: r.str("advisor"),
			CreatedAt: r.timestamp("created_at"),
		}, nil
	case TypeRunnerBound:
		return RunnerBound{
			Runner: r.str("runner"), Service: r.str("service"), BoxID: r.str("box_id"), Baseline: r.str("baseline"),
		}, nil
	case TypeTurnStarted:
		return TurnStarted{TurnID: r.str("turn_id")}, nil
	case TypeTurnCompleted:
		return TurnCompleted{TurnID: r.str("turn_id"), Status: r.str("status"), Error: r.str("error")}, nil
	case TypeUserMessage:
		return decodeUserMessage(r), nil
	default:
		return nil, fmt.Errorf("unsupported session event %q", kind)
	}
}

func decodeUserMessage(r *objectReader) UserMessage {
	var notices []Notice
	var mentions []Mention
	r.array("notices", func(raw jsontext.Value) error {
		value, err := decodeNotice(raw)
		notices = append(notices, value)
		return err
	})
	r.array("mentions", func(raw jsontext.Value) error {
		value, err := decodeMention(raw)
		mentions = append(mentions, value)
		return err
	})
	text, textBlob := r.textPayload("text")
	return UserMessage{
		ItemID: r.str("item_id"), TurnID: r.str("turn_id"), Text: text, TextBlob: textBlob, Kind: r.str("kind"),
		Notices: notices, Mentions: mentions, Effort: r.str("effort"), Verbatim: r.boolean("verbatim"),
		Model: r.str("model"), Approval: r.str("approval"), Sandbox: r.str("sandbox"),
	}
}

func decodeNotice(raw jsontext.Value) (Notice, error) {
	fields, err := decodeObject(raw)
	if err != nil {
		return Notice{}, err
	}
	r := objectReader{fields: fields}
	text, textBlob := r.textPayload("text")
	value := Notice{Word: r.str("word"), Text: text, TextBlob: textBlob}
	return value, r.err
}

func decodeMention(raw jsontext.Value) (Mention, error) {
	fields, err := decodeObject(raw)
	if err != nil {
		return Mention{}, err
	}
	r := objectReader{fields: fields}
	text, textBlob := r.textPayload("text")
	value := Mention{Path: r.str("path"), Text: text, TextBlob: textBlob}
	return value, r.err
}

func decodeToolEvent(kind string, r *objectReader) (Event, error) {
	switch kind {
	case TypeAssistantMessage:
		return decodeAssistantMessage(r), nil
	case TypeToolStarted:
		return decodeToolStarted(r), nil
	case TypeToolCompleted:
		return decodeToolCompleted(r), nil
	case TypeModelRetry:
		return ModelRetry{
			TurnID: r.str("turn_id"), Attempt: r.number("attempt"), Verdict: r.str("verdict"),
			Status: r.number("status"), DelayMS: r.int64("delay_ms"),
		}, nil
	case TypeApprovalRequested:
		return ApprovalRequested{
			RequestID: r.str("request_id"), CallID: r.str("call_id"), Summary: r.str("summary"), Class: r.str("class"),
		}, nil
	case TypeApprovalResolved:
		return ApprovalResolved{
			RequestID: r.str("request_id"), Decision: r.str("decision"), By: r.str("by"),
			Reason: r.str("reason"), Scores: r.scores("scores"),
		}, nil
	default:
		return nil, fmt.Errorf("unsupported tool event %q", kind)
	}
}

func decodeAssistantMessage(r *objectReader) AssistantMessage {
	var calls []ToolCall
	r.array("tool_calls", func(raw jsontext.Value) error {
		value, err := decodeToolCall(raw)
		calls = append(calls, value)
		return err
	})
	usage, err := decodeUsage(r.raw("usage"))
	if r.err == nil {
		r.err = err
	}
	text, textBlob := r.textPayload("text")
	reasoning, reasoningBlob := r.textPayload("reasoning")
	return AssistantMessage{
		ItemID: r.str("item_id"), TurnID: r.str("turn_id"), Text: text, TextBlob: textBlob,
		Reasoning: reasoning, ReasoningBlob: reasoningBlob, ToolCalls: calls, Usage: usage, StopReason: r.str("stop_reason"),
		ContextSeq: r.uint64("context_seq"),
	}
}

func decodeToolCall(raw jsontext.Value) (ToolCall, error) {
	fields, err := decodeObject(raw)
	if err != nil {
		return ToolCall{}, err
	}
	r := objectReader{fields: fields}
	args, argsBlob := r.rawPayload("args")
	value := ToolCall{
		ID: r.str("id"), Name: r.str("name"), Args: args, ArgsBlob: argsBlob,
		Freeform: r.boolean("freeform"), Resolved: r.boolean("resolved"),
	}
	return value, r.err
}

func decodeUsage(raw jsontext.Value) (Usage, error) {
	if len(raw) == 0 {
		return Usage{}, nil
	}
	fields, err := decodeObject(raw)
	if err != nil {
		return Usage{}, err
	}
	r := objectReader{fields: fields}
	value := Usage{
		Input: r.number("input"), Output: r.number("output"),
		CacheRead: r.number("cache_read"), CacheWrite: r.number("cache_write"),
	}
	return value, r.err
}

func decodeToolStarted(r *objectReader) ToolStarted {
	var files []FileChange
	r.array("files", func(raw jsontext.Value) error {
		value, err := decodeFileChange(raw)
		files = append(files, value)
		return err
	})
	args, argsBlob := r.rawPayload("args")
	return ToolStarted{
		CallID: r.str("call_id"), TurnID: r.str("turn_id"), Tool: r.str("tool"), Args: args, ArgsBlob: argsBlob,
		Class: r.str("class"), Repairs: r.number("repairs"), Files: files,
	}
}

func decodeFileChange(raw jsontext.Value) (FileChange, error) {
	fields, err := decodeObject(raw)
	if err != nil {
		return FileChange{}, err
	}
	r := objectReader{fields: fields}
	value := FileChange{
		Path: r.str("path"), PreSHA: r.str("pre_sha"), PostSHA: r.str("post_sha"), PreBlob: r.blob("pre_blob"),
	}
	return value, r.err
}

func decodeToolCompleted(r *objectReader) ToolCompleted {
	var spill *Spill
	if raw, ok := r.fields["spill"]; ok {
		value, err := decodeSpill(raw)
		spill = &value
		r.err = err
	}
	var exitCode *int
	if _, ok := r.fields["exit_code"]; ok {
		value := r.number("exit_code")
		exitCode = &value
	}
	return ToolCompleted{
		CallID: r.str("call_id"), OK: r.boolean("ok"), Output: r.str("output"), Spill: spill,
		ExitCode: exitCode, Recovered: r.str("recovered"), Meta: r.raw("meta"),
	}
}

func decodeSpill(raw jsontext.Value) (Spill, error) {
	fields, err := decodeObject(raw)
	if err != nil {
		return Spill{}, err
	}
	r := objectReader{fields: fields}
	value := Spill{Path: r.str("path"), Bytes: r.int64("bytes"), SHA256: r.str("sha256")}
	return value, r.err
}

func decodeStateEvent(kind string, r *objectReader) (Event, error) {
	switch kind {
	case TypeCompaction:
		return Compaction{
			FirstKeptSeq: r.uint64("first_kept_seq"), Summary: r.str("summary"), Method: r.str("method"),
			TokensBefore: r.number("tokens_before"), TokensAfter: r.number("tokens_after"),
		}, nil
	case TypeContextPruned:
		return decodeContextPruned(r), nil
	case TypeChildSpawned:
		return ChildSpawned{CallID: r.str("call_id"), ChildID: r.str("child_id"), Runner: r.str("runner")}, nil
	case TypeBranch:
		return Branch{ParentSeq: r.uint64("parent_seq")}, nil
	default:
		return nil, fmt.Errorf("unsupported state event %q", kind)
	}
}

func decodeContextPruned(r *objectReader) ContextPruned {
	var ids []string
	r.array("call_ids", func(raw jsontext.Value) error {
		value, err := decodeString(raw)
		ids = append(ids, value)
		return err
	})
	return ContextPruned{TurnID: r.str("turn_id"), CallIDs: ids}
}

func decodeScriptEvent(kind string, r *objectReader) (Event, error) {
	switch kind {
	case TypeScriptStarted:
		return decodeScriptStarted(r), nil
	case TypeScriptEffect:
		result, resultBlob := r.rawPayload("result")
		return ScriptEffect{
			RunID: r.str("run_id"), N: r.uint64("n"), Op: r.str("op"),
			ArgsSHA: r.str("args_sha"), Result: result, ResultBlob: resultBlob, Error: r.str("error"),
		}, nil
	case TypeScriptCompleted:
		value, valueBlob := r.rawPayload("value")
		return ScriptCompleted{
			RunID: r.str("run_id"), OK: r.boolean("ok"), Value: value, ValueBlob: valueBlob, Error: r.str("error"),
		}, nil
	default:
		return nil, fmt.Errorf("unsupported script event %q", kind)
	}
}

func decodeScriptStarted(r *objectReader) ScriptStarted {
	blob := r.blob("source_blob")
	var source BlobRef
	if blob != nil {
		source = *blob
	}
	args, argsBlob := r.rawPayload("args")
	return ScriptStarted{
		RunID: r.str("run_id"), SourceBlob: source, Entry: r.str("entry"),
		Args: args, ArgsBlob: argsBlob, Seed: r.uint64("seed"),
	}
}
