package event_test

import (
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

func hashlineFold(meta jsontext.Value) []event.Event {
	return []event.Event{
		event.TurnStarted{TurnID: "t"},
		event.AssistantMessage{
			ItemID: "a", TurnID: "t", StopReason: "tool_calls",
			ToolCalls: []event.ToolCall{{ID: "c", Name: "patch", Args: jsontext.Value(`{}`)}},
		},
		event.ToolStarted{CallID: "c", TurnID: "t", Tool: "patch", Class: "edit", Args: jsontext.Value(`{}`)},
		event.ToolCompleted{CallID: "c", OK: true, Recovered: "none", Meta: meta},
		event.TurnCompleted{TurnID: "t", Status: "completed"},
	}
}

func TestFoldIgnoresHashlineMeta(t *testing.T) {
	t.Parallel()
	outcomes := []tool.Outcome{
		{Style: "hashline-light", Version: 1, Status: tool.StatusApplied, Commit: tool.CommitComplete},
		{
			Style: "hashline-enhanced", Version: 1, Status: tool.StatusNeedsContext, Commit: tool.CommitNone,
			Reason: tool.ReasonUnobservedRange, Class: tool.ErrClassProof,
			Recovery: &tool.Recovery{
				Kind: "read", Path: "src/lib.rs", Ranges: []tool.LineRange{{First: 40, Last: 45}},
				Mapping: "known", Remaining: []tool.LineRange{{First: 60, Last: 61}},
			},
		},
		{
			Style: "hashline-light", Version: 1, Status: tool.StatusWriteError, Commit: tool.CommitPartial,
			Reason: tool.ReasonRollbackFailed, Class: tool.ErrClassIO,
			Paths: []tool.PathState{{Path: "a.txt", State: tool.PathAfter}, {Path: "b.txt", State: tool.PathBefore}},
		},
	}
	for _, outcome := range outcomes {
		t.Run("COMPAT-03/"+string(outcome.Status), func(t *testing.T) {
			t.Parallel()
			meta, err := outcome.Meta(false)
			if err != nil {
				t.Fatal(err)
			}
			state, err := foldEvents(t, hashlineFold(meta)...)
			if err != nil {
				t.Fatalf("Fold rejected a completion carrying the hashline object: %v", err)
			}
			if len(state.Snapshots) != 0 {
				t.Fatalf("hashline object created snapshots: %+v", state.Snapshots)
			}
		})
	}
	t.Run("COMPAT-03/top-level ref, path and blob still create a snapshot", func(t *testing.T) {
		t.Parallel()
		meta := jsontext.Value(`{"ref":"r1","path":"a.go","blob":"` + digest + `","hashline":{"replayed":false}}`)
		state, err := foldEvents(t, hashlineFold(meta)...)
		if err != nil {
			t.Fatal(err)
		}
		if _, ok := state.Snapshots["r1"]; !ok {
			t.Fatalf("snapshots = %+v, want r1", state.Snapshots)
		}
	})
}
