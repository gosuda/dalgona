package event_test

import (
	"encoding/json/jsontext"
	"maps"
	"reflect"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestFoldRejectsIllegalTransitions(t *testing.T) {
	t.Parallel()
	started := event.ToolStarted{CallID: "c", TurnID: "t", Tool: "read", Class: "read", Args: jsontext.Value(`{}`)}
	assistant := event.AssistantMessage{
		ItemID: "a", TurnID: "t", ToolCalls: []event.ToolCall{{ID: "c", Name: "read", Args: jsontext.Value(`{}`)}},
		StopReason: "tool_calls",
	}
	script := event.ScriptStarted{
		RunID: "s", SourceBlob: event.BlobRef{Blob: digest, Bytes: 1}, Entry: "main", Args: jsontext.Value(`{}`),
	}
	cases := []struct {
		name   string
		events []event.Event
	}{
		{name: "completion without start", events: []event.Event{completedCall("c", "none")}},
		{name: "nested turn", events: []event.Event{event.TurnStarted{TurnID: "t"}, event.TurnStarted{TurnID: "u"}}},
		{name: "mismatched turn", events: []event.Event{event.TurnStarted{TurnID: "t"}, event.TurnCompleted{TurnID: "u", Status: "completed"}}},
		{name: "duplicate tool start", events: []event.Event{event.TurnStarted{TurnID: "t"}, started, started}},
		{name: "unfinished approval", events: []event.Event{
			event.TurnStarted{TurnID: "t"},
			assistant,
			event.ApprovalRequested{RequestID: "r", CallID: "c", Summary: "read"},
			event.TurnCompleted{TurnID: "t", Status: "completed"},
		}},
		{name: "script effect duplicate index", events: []event.Event{
			event.TurnStarted{TurnID: "t"},
			script,
			event.ScriptEffect{RunID: "s", N: 1, Op: "time", ArgsSHA: digest, Result: jsontext.Value(`1`)},
			event.ScriptEffect{RunID: "s", N: 1, Op: "time", ArgsSHA: digest, Result: jsontext.Value(`1`)},
		}},
		{name: "script effect op mismatch", events: []event.Event{
			event.TurnStarted{TurnID: "t"},
			script,
			event.ScriptEffect{RunID: "s", N: 0, Op: "time", ArgsSHA: digest, Result: jsontext.Value(`1`)},
			event.ScriptEffect{RunID: "s", N: 0, Op: "sleep", ArgsSHA: digest, Result: jsontext.Value(`1`)},
		}},
		{name: "running branch", events: []event.Event{event.TurnStarted{TurnID: "t"}, event.Branch{}}},
		{name: "unknown branch parent", events: []event.Event{
			event.TurnStarted{TurnID: "t"}, event.TurnCompleted{TurnID: "t", Status: "completed"}, event.Branch{ParentSeq: 1},
		}},
		{name: "duplicate assistant call", events: []event.Event{event.TurnStarted{TurnID: "t"}, assistant, assistant}},
		{name: "pending call at turn end", events: []event.Event{
			event.TurnStarted{TurnID: "t"}, assistant, event.TurnCompleted{TurnID: "t", Status: "completed"},
		}},
		{name: "script completion without start", events: []event.Event{
			event.TurnStarted{TurnID: "t"}, event.ScriptCompleted{RunID: "s", Error: "bad"},
		}},
		{name: "unknown child call", events: []event.Event{
			event.TurnStarted{TurnID: "t"}, event.ChildSpawned{CallID: "c", ChildID: "child", Runner: "local"},
		}},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			_, err := foldEvents(t, event.State{}, test.events...)
			if err == nil {
				t.Fatal("Fold accepted an illegal transition")
			}
		})
	}
}

func TestFoldTracksRecoveryState(t *testing.T) {
	t.Parallel()
	meta := jsontext.Value(`{"ref":"r1","path":"a.go","blob":"` + digest + `","shown":[[1,3]],"revision":2,"epoch":4}`)
	todoArgs := jsontext.Value(`{"items":[{"text":"ship","status":"in_progress"}]}`)
	assistant := event.AssistantMessage{
		ItemID: "a", TurnID: "t", StopReason: "tool_calls", ToolCalls: []event.ToolCall{
			{ID: "read", Name: "read", Args: jsontext.Value(`{}`)},
			{ID: "child", Name: "task", Args: jsontext.Value(`{}`)},
			{ID: "script", Name: "eval", Args: jsontext.Value(`{}`)},
			{ID: "todo", Name: "todo", Args: todoArgs},
		},
	}
	events := []event.Event{
		event.SessionCreated{ID: "s", Cwd: "/work", Model: "p/m", Approval: "auto", Sandbox: "workspace-write", Runner: "remote"},
		event.RunnerBound{Runner: "remote", Service: "docker", BoxID: "box", Baseline: "base"},
		event.TurnStarted{TurnID: "t"},
		event.UserMessage{ItemID: "u", TurnID: "t", Text: "work", Kind: "prompt"},
		assistant,
		event.ToolStarted{CallID: "read", TurnID: "t", Tool: "read", Class: "read", Args: jsontext.Value(`{}`)},
		event.ToolCompleted{CallID: "read", OK: true, Recovered: "none", Meta: meta},
		event.ApprovalRequested{RequestID: "approval", CallID: "child", Summary: "spawn"},
		event.ApprovalResolved{RequestID: "approval", Decision: "accept", By: "decider:test", Reason: "approved"},
		event.ToolStarted{CallID: "child", TurnID: "t", Tool: "task", Class: "agent", Args: jsontext.Value(`{}`)},
		event.ChildSpawned{CallID: "child", ChildID: "child-session", Runner: "remote"},
		completedCall("child", "none"),
		event.ToolStarted{CallID: "script", TurnID: "t", Tool: "eval", Class: "script", Args: jsontext.Value(`{}`)},
		event.ScriptStarted{RunID: "script", SourceBlob: event.BlobRef{Blob: digest, Bytes: 5}, Entry: "main", Args: jsontext.Value(`{}`), Seed: 9},
		event.ScriptEffect{RunID: "script", N: 0, Op: "time", ArgsSHA: digest, Result: jsontext.Value(`1`)},
		event.ScriptCompleted{RunID: "script", OK: true, Value: jsontext.Value(`1`)},
		completedCall("script", "none"),
		event.ToolStarted{CallID: "todo", TurnID: "t", Tool: "todo", Class: "read", Args: todoArgs},
		completedCall("todo", "none"),
		event.ModelRetry{TurnID: "t", Attempt: 1, Verdict: "retry", DelayMS: 1},
		event.ContextPruned{TurnID: "t", CallIDs: []string{"read"}},
		event.Compaction{FirstKeptSeq: 2, Summary: "summary", Method: "auto", TokensBefore: 10, TokensAfter: 5},
		event.TurnCompleted{TurnID: "t", Status: "completed"},
	}
	state, err := foldEvents(t, event.State{}, events...)
	if err != nil {
		t.Fatalf("Fold valid history: %v", err)
	}
	completionSeq := uint64(len(events))
	assertRecoveryState(t, state, completionSeq)
	_, err = event.Fold(state, completionSeq+1, event.Branch{ParentSeq: completionSeq})
	if err != nil {
		t.Fatalf("Fold valid branch: %v", err)
	}
}

func assertRecoveryState(t *testing.T, state event.State, completionSeq uint64) {
	t.Helper()
	checks := []struct {
		name string
		ok   bool
	}{
		{name: "session status", ok: state.Status == event.StatusIdle && state.SessionID == "s" && state.TurnID == ""},
		{name: "runner", ok: state.Runner != nil && state.Runner.BoxID == "box"},
		{name: "compaction", ok: state.LastCompaction != nil},
		{name: "snapshot", ok: state.Snapshots["r1"].Revision == 2},
		{name: "child", ok: state.Children["child-session"].CallID == "child"},
		{name: "todo", ok: len(state.Todos) == 1 && state.Todos[0].Status == "in_progress"},
		{name: "unfinished work", ok: len(state.InFlight)+len(state.PendingCalls)+len(state.Approvals)+len(state.Scripts) == 0},
		{name: "completion sequence", ok: state.CompletedTurns["t"] == completionSeq},
	}
	for _, check := range checks {
		if !check.ok {
			t.Errorf("%s mismatch: %#v", check.name, state)
		}
	}
}

func TestFoldCompletesSkippedAndDeclinedCalls(t *testing.T) {
	t.Parallel()
	assistant := event.AssistantMessage{
		ItemID: "a", TurnID: "t", StopReason: "tool_calls",
		ToolCalls: []event.ToolCall{{ID: "c", Name: "exec", Args: jsontext.Value(`{}`)}},
	}
	prefix := []event.Event{event.TurnStarted{TurnID: "t"}, assistant}
	skipped := completedCall("c", "skipped")
	skipped.OK = false
	_, err := foldEvents(t, event.State{}, append(prefix, skipped, event.TurnCompleted{TurnID: "t", Status: "interrupted"})...)
	if err != nil {
		t.Fatalf("Fold skipped call: %v", err)
	}
	declined := completedCall("c", "none")
	declined.OK = false
	declinedEvents := append(slices.Clone(prefix),
		event.ApprovalRequested{RequestID: "r", CallID: "c", Summary: "run"},
		event.ApprovalResolved{RequestID: "r", Decision: "decline", By: "rule:test", Reason: "denied"},
		declined, event.TurnCompleted{TurnID: "t", Status: "completed"},
	)
	state, err := foldEvents(t, event.State{}, declinedEvents...)
	if err != nil || state.Status != event.StatusIdle {
		t.Fatalf("Fold declined call: state=%#v err=%v", state, err)
	}
}

func TestFoldTracksUserSessionGrants(t *testing.T) {
	t.Parallel()
	cases := []struct {
		tool  string
		class string
		args  jsontext.Value
		want  string
	}{
		{tool: "exec", class: "exec", args: jsontext.Value(`{"command":"go test ./..."}`), want: "go test"},
		{tool: "patch", class: "edit", args: jsontext.Value(`{}`), want: "tool:patch"},
		{tool: "server__remote", class: "remote", args: jsontext.Value(`{}`), want: "tool:server__remote"},
		{tool: "task", class: "agent", args: jsontext.Value(`{}`)},
	}
	for _, test := range cases {
		t.Run(test.tool, func(t *testing.T) {
			t.Parallel()
			assistant := event.AssistantMessage{
				ItemID: "a", TurnID: "t", StopReason: "tool_calls",
				ToolCalls: []event.ToolCall{{ID: "c", Name: test.tool, Args: test.args}},
			}
			resolved, err := foldEvents(t, event.State{},
				event.TurnStarted{TurnID: "t"}, assistant,
				event.ApprovalRequested{RequestID: "r", CallID: "c", Summary: "run"},
				event.ApprovalResolved{RequestID: "r", Decision: "accept_session", By: "user"},
			)
			if err != nil {
				t.Fatalf("Fold grant: %v", err)
			}
			assertAcceptedCall(t, resolved)
			state, err := event.Fold(resolved, 5, event.ToolStarted{CallID: "c", TurnID: "t", Tool: test.tool, Class: test.class, Args: test.args})
			if err != nil {
				t.Fatalf("Fold start: %v", err)
			}
			assertGrant(t, state.Grants, test.want, test.tool)
		})
	}
}

func TestFoldGrantSurvivesCallSkippedAfterCrash(t *testing.T) {
	t.Parallel()
	cases := []struct {
		tool, class, args, want string
	}{
		{tool: "exec", class: "exec", args: `{"command":"go test ./..."}`, want: "go test"},
		{tool: "patch", class: "edit", args: `{}`, want: "tool:patch"},
		{tool: "task", class: "agent", args: `{}`},
	}
	for _, test := range cases {
		assistant := event.AssistantMessage{
			ItemID: "a", TurnID: "t", StopReason: "tool_calls",
			ToolCalls: []event.ToolCall{{ID: "c", Name: test.tool, Args: jsontext.Value(test.args)}},
		}
		state, err := foldEvents(t, event.State{},
			event.TurnStarted{TurnID: "t"}, assistant,
			event.ApprovalRequested{RequestID: "r", CallID: "c", Summary: "run", Class: test.class},
			event.ApprovalResolved{RequestID: "r", Decision: "accept_session", By: "user"},
			event.ToolCompleted{CallID: "c", Output: "skipped: the host restarted before this call ran", Recovered: "skipped"},
			event.TurnCompleted{TurnID: "t", Status: "completed"},
		)
		if err != nil {
			t.Fatalf("%s: %v", test.tool, err)
		}
		if len(state.SessionAccepted) != 0 {
			t.Fatalf("%s: accepted calls left behind: %#v", test.tool, state.SessionAccepted)
		}
		assertGrant(t, state.Grants, test.want, test.tool)
	}
}

func assertAcceptedCall(t *testing.T, state event.State) {
	t.Helper()
	if _, kept := state.SessionAccepted["c"]; !kept || len(state.Grants) != 0 {
		t.Fatalf("a replay that stops before tool.started must keep the accepted call: %#v %#v", state.SessionAccepted, state.Grants)
	}
}

func assertGrant(t *testing.T, grants map[string]string, want, toolName string) {
	t.Helper()
	if want == "" && len(grants) != 0 {
		t.Fatalf("unexpected grants: %#v", grants)
	}
	if want != "" && grants[want] != toolName {
		t.Fatalf("grants = %#v, want %s for %s", grants, want, toolName)
	}
}

func TestFoldAcceptsOutOfOrderScriptEffects(t *testing.T) {
	t.Parallel()
	script := event.ScriptStarted{RunID: "s", SourceBlob: event.BlobRef{Blob: digest, Bytes: 1}, Entry: "main", Args: jsontext.Value(`{}`)}
	effect := func(n uint64) event.Event {
		return event.ScriptEffect{RunID: "s", N: n, Op: "tool", ArgsSHA: digest, Result: jsontext.Value(`1`)}
	}
	state, err := foldEvents(t, event.State{}, event.TurnStarted{TurnID: "t"}, script, effect(0), effect(2), effect(1))
	if err != nil {
		t.Fatalf("out-of-order effects: %v", err)
	}
	if len(state.Scripts["s"].Seen) != 3 {
		t.Fatalf("script state = %#v", state.Scripts["s"])
	}
	for _, n := range []uint64{1, 2} {
		if _, err := event.Fold(state, 99, effect(n)); err == nil {
			t.Fatalf("effect %d accepted after out-of-order completion", n)
		}
	}
}

func TestFoldFailureDoesNotMutateInput(t *testing.T) {
	t.Parallel()
	original := event.State{
		Status: event.StatusTurnRunning, TurnID: "t",
		InFlight:     map[string]event.ToolState{"c": {CallID: "c", Class: "read"}},
		PendingCalls: map[string]event.ToolCall{"p": {ID: "p", Name: "read", Args: jsontext.Value(`{}`)}},
		Todos:        []event.TodoItem{{Text: "work", Status: "pending"}},
	}
	inFlight := maps.Clone(original.InFlight)
	pending := maps.Clone(original.PendingCalls)
	todos := slices.Clone(original.Todos)
	damaged, err := event.Fold(original, 9, completedCall("missing", "none"))
	if err == nil || damaged.Status != event.StatusDamaged {
		t.Fatalf("Fold failure = %#v, %v", damaged, err)
	}
	if !reflect.DeepEqual(original.InFlight, inFlight) || !reflect.DeepEqual(original.PendingCalls, pending) || !reflect.DeepEqual(original.Todos, todos) {
		t.Fatalf("Fold mutated its input: %#v", original)
	}
	if original.Status != event.StatusTurnRunning {
		t.Fatalf("input status changed to %s", original.Status)
	}
}

const digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

func completedCall(id, recovered string) event.ToolCompleted {
	return event.ToolCompleted{CallID: id, OK: true, Recovered: recovered}
}

func foldEvents(t *testing.T, initial event.State, events ...event.Event) (event.State, error) {
	t.Helper()
	state := initial
	var err error
	for i, current := range events {
		state, err = event.Fold(state, uint64(i+1), current)
		if err != nil {
			return state, err
		}
	}
	return state, nil
}
