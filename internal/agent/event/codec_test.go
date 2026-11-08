package event_test

import (
	"bytes"
	"encoding/json/jsontext"
	"fmt"
	"math/rand/v2"
	"reflect"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestCodecRoundTripRandomEvents(t *testing.T) {
	t.Parallel()
	random := rand.New(rand.NewPCG(20261108, 1))
	for iteration := range 100 {
		for _, original := range randomEvents(random) {
			assertRoundTrip(t, iteration, original)
		}
	}
}

func assertRoundTrip(t *testing.T, iteration int, original event.Event) {
	t.Helper()
	encoded, err := event.Encode(original)
	if err != nil {
		t.Fatalf("iteration %d encode %s: %v", iteration, original.Type(), err)
	}
	if bytes.HasSuffix(encoded, []byte{'\n'}) {
		t.Fatalf("iteration %d %s has a journal newline", iteration, original.Type())
	}
	decoded, err := event.Decode(encoded)
	if err != nil {
		t.Fatalf("iteration %d decode %s: %v\n%s", iteration, original.Type(), err, encoded)
	}
	if !reflect.DeepEqual(decoded, original) {
		t.Fatalf("iteration %d %s mismatch\ngot:  %#v\nwant: %#v", iteration, original.Type(), decoded, original)
	}
}

func TestCodecRoundTripBlobPayloads(t *testing.T) {
	t.Parallel()
	blob := &event.BlobRef{Blob: digest, Bytes: 100000}
	calls := []event.ToolCall{{ID: "c", Name: "write", ArgsBlob: blob}}
	candidates := []event.Event{
		event.UserMessage{
			ItemID: "u", TurnID: "t", TextBlob: blob, Kind: "prompt",
			Notices:  []event.Notice{{Word: "large", TextBlob: blob}},
			Mentions: []event.Mention{{Path: "large.txt", TextBlob: blob}},
		},
		event.AssistantMessage{
			ItemID: "a", TurnID: "t", TextBlob: blob, ReasoningBlob: blob,
			ToolCalls: calls, StopReason: "tool_calls",
		},
		event.ToolStarted{CallID: "c", TurnID: "t", Tool: "write", Class: "edit", ArgsBlob: blob},
		event.ScriptStarted{RunID: "s", SourceBlob: *blob, Entry: "main", ArgsBlob: blob},
		event.ScriptEffect{RunID: "s", Op: "tool", ArgsSHA: digest, ResultBlob: blob},
		event.ScriptCompleted{RunID: "s", OK: true, ValueBlob: blob},
	}
	for _, original := range candidates {
		encoded, err := event.Encode(original)
		if err != nil {
			t.Fatalf("Encode(%s): %v", original.Type(), err)
		}
		decoded, err := event.Decode(encoded)
		if err != nil {
			t.Fatalf("Decode(%s): %v", original.Type(), err)
		}
		if !reflect.DeepEqual(decoded, original) {
			t.Fatalf("%s blob mismatch: got %#v want %#v", original.Type(), decoded, original)
		}
	}
}

func TestDecodeRejectsUnknownAndMalformedEvents(t *testing.T) {
	t.Parallel()
	inputs := []string{
		`{"type":"unknown.event"}`,
		`{"type":7}`,
		`[]`,
		`{"type":"turn.started","turn_id":"t"} trailing`,
		`{"type":"turn.started","turn_id":7}`,
		`{"type":"turn.started"}`,
	}
	for _, input := range inputs {
		if _, err := event.Decode([]byte(input)); err == nil {
			t.Errorf("Decode(%s) succeeded", input)
		}
	}
}

func TestEncodeRejectsInvalidEvent(t *testing.T) {
	t.Parallel()
	if _, err := event.Encode(event.TurnStarted{}); err == nil {
		t.Fatal("Encode accepted an empty turn id")
	}
	if _, err := event.Encode(nil); err == nil {
		t.Fatal("Encode accepted nil")
	}
}

func randomEvents(r *rand.Rand) []event.Event {
	return []event.Event{
		event.SessionCreated{
			ID: text(r), Parent: text(r), ForkOf: text(r), Cwd: text(r), Model: text(r),
			Approval: pick(r, "auto", "yolo", "ask"), Sandbox: pick(r, "workspace-write", "off"),
			Runner: text(r), InstructionsBlob: optionalBlob(r),
		},
		event.RunnerBound{Runner: text(r), Service: text(r), BoxID: text(r), Baseline: text(r)},
		event.TurnStarted{TurnID: text(r)},
		event.TurnCompleted{TurnID: text(r), Status: pick(r, "completed", "interrupted", "failed"), Error: text(r)},
		event.UserMessage{
			ItemID: text(r), TurnID: text(r), Text: text(r), Kind: pick(r, "prompt", "steer", "followup"),
			Notices: randomNotices(r), Mentions: randomMentions(r),
		},
		event.AssistantMessage{
			ItemID: text(r), TurnID: text(r), Text: text(r), Reasoning: text(r),
			ToolCalls: randomToolCalls(r), Usage: event.Usage{
				Input: positive(r), Output: positive(r), CacheRead: positive(r), CacheWrite: positive(r),
			}, StopReason: text(r),
		},
		event.ToolStarted{
			CallID: text(r), TurnID: text(r), Tool: text(r), Args: raw(r),
			Class:   pick(r, "read", "edit", "exec", "agent", "script", "remote"),
			Repairs: positive(r), Files: randomFiles(r),
		},
		event.ToolCompleted{
			CallID: text(r), OK: r.IntN(2) == 0, Output: text(r), Spill: optionalSpill(r),
			ExitCode: optionalExit(r), Recovered: pick(r, "none", "replayed", "interrupted", "skipped"), Meta: raw(r),
		},
		event.ModelRetry{
			TurnID: text(r), Attempt: positive(r) + 1, Verdict: pick(r, "retry", "overflow"),
			Status: positive(r), DelayMS: int64(positive(r)),
		},
		event.ApprovalRequested{RequestID: text(r), CallID: text(r), Summary: text(r)},
		event.ApprovalResolved{
			RequestID: text(r), Decision: pick(r, "accept", "accept_session", "decline", "cancel"),
			By: text(r), Reason: text(r), Scores: map[string]float64{text(r): r.Float64(), text(r): r.Float64()},
		},
		event.Compaction{
			FirstKeptSeq: r.Uint64N(1000000) + 1, Summary: text(r), Method: pick(r, "auto", "manual", "overflow"),
			TokensBefore: positive(r), TokensAfter: positive(r),
		},
		event.ContextPruned{TurnID: text(r), CallIDs: []string{text(r), text(r)}},
		event.ChildSpawned{CallID: text(r), ChildID: text(r), Runner: text(r)},
		event.Branch{ParentSeq: r.Uint64N(1000000)},
		event.ScriptStarted{RunID: text(r), SourceBlob: blob(r), Entry: text(r), Args: raw(r), Seed: r.Uint64()},
		event.ScriptEffect{RunID: text(r), N: r.Uint64(), Op: text(r), ArgsSHA: sha(r), Result: raw(r)},
		randomScriptCompleted(r),
	}
}

func randomScriptCompleted(r *rand.Rand) event.ScriptCompleted {
	if r.IntN(2) == 0 {
		return event.ScriptCompleted{RunID: text(r), OK: true, Value: raw(r)}
	}
	return event.ScriptCompleted{RunID: text(r), Error: text(r)}
}

func randomNotices(r *rand.Rand) []event.Notice {
	items := make([]event.Notice, 1+r.IntN(3))
	for i := range items {
		items[i] = event.Notice{Word: text(r), Text: text(r)}
	}
	return items
}

func randomMentions(r *rand.Rand) []event.Mention {
	items := make([]event.Mention, 1+r.IntN(3))
	for i := range items {
		items[i] = event.Mention{Path: text(r), Text: text(r)}
	}
	return items
}

func randomToolCalls(r *rand.Rand) []event.ToolCall {
	items := make([]event.ToolCall, 1+r.IntN(3))
	for i := range items {
		items[i] = event.ToolCall{
			ID: text(r), Name: text(r), Args: raw(r), Freeform: r.IntN(2) == 0, Resolved: r.IntN(2) == 0,
		}
	}
	return items
}

func randomFiles(r *rand.Rand) []event.FileChange {
	items := make([]event.FileChange, 1+r.IntN(3))
	for i := range items {
		items[i] = event.FileChange{Path: text(r), PreSHA: sha(r), PostSHA: sha(r), PreBlob: optionalBlob(r)}
	}
	return items
}

func optionalBlob(r *rand.Rand) *event.BlobRef {
	if r.IntN(2) == 0 {
		return nil
	}
	value := blob(r)
	return &value
}

func blob(r *rand.Rand) event.BlobRef {
	return event.BlobRef{Blob: sha(r), Bytes: int64(positive(r))}
}

func optionalSpill(r *rand.Rand) *event.Spill {
	if r.IntN(2) == 0 {
		return nil
	}
	return &event.Spill{Path: text(r), Bytes: int64(positive(r)), SHA256: sha(r)}
}

func optionalExit(r *rand.Rand) *int {
	if r.IntN(2) == 0 {
		return nil
	}
	value := r.IntN(256) - 128
	return &value
}

func raw(r *rand.Rand) jsontext.Value {
	return jsontext.Value(fmt.Sprintf(`{"value":%d,"text":%q}`, positive(r), text(r)))
}

func sha(r *rand.Rand) string {
	const digits = "0123456789abcdef"
	var out strings.Builder
	out.Grow(64)
	for range 64 {
		out.WriteByte(digits[r.IntN(len(digits))])
	}
	return out.String()
}

func text(r *rand.Rand) string {
	return fmt.Sprintf("txt-%d-\"\\-달", r.Int64())
}

func positive(r *rand.Rand) int {
	return r.IntN(1000000)
}

func pick[T any](r *rand.Rand, values ...T) T {
	return values[r.IntN(len(values))]
}
