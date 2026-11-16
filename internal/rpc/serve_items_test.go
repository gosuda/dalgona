package rpc

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"log/slog"
	"math/rand/v2"
	"reflect"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

var itemKinds = []string{kindUser, kindAgent, kindReasoning, kindCommand, kindFileChange, kindMCP, kindCompaction, kindDynamic}

func randomText(r *rand.Rand) string {
	alphabet := []string{"a", "b", " ", "\n", "\"", "\\", "é", "\x1b", "<", "{"}
	var b strings.Builder
	for range r.IntN(12) {
		b.WriteString(alphabet[r.IntN(len(alphabet))])
	}
	return b.String()
}

func randomItem(r *rand.Rand) Item {
	it := Item{Type: itemKinds[r.IntN(len(itemKinds))], ID: "item-" + randomText(r) + "x"}
	statuses := []string{statusInProgress, statusCompleted, statusFailed, statusDeclined}
	switch it.Type {
	case kindUser, kindAgent, kindReasoning:
		it.Text = randomText(r)
	case kindCommand:
		code := r.IntN(3) - 1
		it.Command, it.Cwd, it.Status, it.ExitCode = randomText(r), "/w", statuses[r.IntN(4)], &code
		if it.Status != statusInProgress {
			it.Output = randomText(r)
		}
	case kindFileChange:
		it.Status = statuses[r.IntN(4)]
		for range r.IntN(3) {
			it.Changes = append(it.Changes, FileChange{Path: "f" + randomText(r), Kind: []string{kindAdd, kindUpdate, kindDelete}[r.IntN(3)], Diff: randomText(r)})
		}
	case kindMCP, kindDynamic:
		it.Tool, it.Status, it.Arguments = "srv__"+strings.ReplaceAll(randomText(r), "_", ""), statuses[r.IntN(3)], jsontext.Value(`{"k":1}`)
		if it.Status != statusInProgress {
			it.Output = randomText(r)
		}
	}
	return it
}

func TestItemCodecRoundTripsAndValidates(t *testing.T) {
	r := rand.New(rand.NewPCG(7, 11))
	for i := range 400 {
		original := randomItem(r)
		if original.Type == kindMCP {
			original.Server, original.Tool, _ = strings.Cut(original.Tool, "__")
		}
		raw, err := encode(ItemCompletedNotification{ThreadID: "t", TurnID: "u", CompletedAtMs: 1, Item: original})
		if err != nil {
			t.Fatal(err)
		}
		validateLine(t, "v2/ItemCompletedNotification.json", raw)
		var decoded ItemCompletedNotification
		if err := json.Unmarshal(raw, &decoded); err != nil {
			t.Fatalf("case %d: decode %s: %v", i, raw, err)
		}
		want := original
		if want.Status == statusDeclined && (want.Type == kindMCP || want.Type == kindDynamic) {
			want.Status = statusFailed
		}
		if !reflect.DeepEqual(normalize(decoded.Item), normalize(want)) {
			t.Fatalf("case %d: %s\ndecoded %+v\nwant    %+v", i, raw, decoded.Item, want)
		}
	}
}

func normalize(it Item) Item {
	if len(it.Changes) == 0 {
		it.Changes = nil
	}
	if len(it.Arguments) == 0 || string(it.Arguments) == "null" {
		it.Arguments = nil
	}
	return it
}

func TestItemDecodeIgnoresMemberOrder(t *testing.T) {
	for _, tc := range []struct {
		raw  string
		want Item
	}{
		{`{"content":[{"type":"text","text":"hi"}],"id":"u","type":"userMessage"}`, Item{Type: kindUser, ID: "u", Text: "hi"}},
		{`{"summary":["think"],"content":[],"type":"reasoning","id":"r"}`, Item{Type: kindReasoning, ID: "r", Text: "think"}},
		{`{"contentItems":[{"type":"inputText","text":"out"}],"tool":"todo","type":"dynamicToolCall","id":"d","status":"completed"}`, Item{Type: kindDynamic, ID: "d", Tool: "todo", Status: statusCompleted, Output: "out"}},
	} {
		var got Item
		if err := json.Unmarshal([]byte(tc.raw), &got); err != nil {
			t.Fatal(err)
		}
		if !reflect.DeepEqual(got, tc.want) {
			t.Fatalf("%s decoded %+v, want %+v", tc.raw, got, tc.want)
		}
	}
}

type planSink struct {
	items   []string
	methods []string
	notes   []func() writer
}

func (s *planSink) turn(string, Turn) {}
func (s *planSink) item(method, _ string, it *Item) {
	if method == methodItemStarted {
		s.items = append(s.items, it.ID+" "+it.Type)
	}
}

func (s *planSink) notify(method string, build func() writer) {
	s.methods = append(s.methods, method)
	s.notes = append(s.notes, build)
}

func newPlanTracker(sink *planSink) *tracker {
	tr := newTracker("t1", nil, nil, nil, slog.New(slog.DiscardHandler))
	tr.sink = sink
	return tr
}

func TestNestedCallWireKindAndID(t *testing.T) {
	t.Parallel()
	sink := &planSink{}
	tr := newPlanTracker(sink)
	tr.apply(1, event.ToolStarted{CallID: "e1", TurnID: "turn-1", Tool: "eval", Class: "script", Args: jsontext.Value(`{"code":"1"}`)})
	tr.apply(2, event.ToolStarted{CallID: "e1/1", TurnID: "turn-1", Tool: "read", Class: "read", Args: jsontext.Value(`{"path":"a.go"}`)})
	tr.apply(3, event.ToolStarted{CallID: "e1/5", TurnID: "turn-1", Tool: "exec", Class: "exec", Args: jsontext.Value(`{"command":"ls"}`)})
	want := []string{"e1 dynamicToolCall", "e1/1 dynamicToolCall", "e1/5 commandExecution"}
	if !slices.Equal(sink.items, want) {
		t.Fatalf("items = %v, want %v", sink.items, want)
	}
}

func TestNestedTodoEmitsPlanUpdate(t *testing.T) {
	t.Parallel()
	sink := &planSink{}
	tr := newPlanTracker(sink)
	tr.apply(1, event.ToolStarted{CallID: "e1/2", TurnID: "turn-1", Tool: "todo", Class: "read", Args: jsontext.Value(`{"items":[{"text":"read the test","status":"in_progress"}]}`)})
	tr.apply(2, event.ToolCompleted{CallID: "e1/2", OK: true})
	index := slices.Index(sink.methods, "turn/plan/updated")
	if index < 0 {
		t.Fatalf("notifications = %v, want turn/plan/updated", sink.methods)
	}
	plan, ok := sink.notes[index]().(TurnPlanUpdatedNotification)
	if !ok || len(plan.Plan) != 1 || plan.Plan[0].Status != statusInProgress {
		t.Fatalf("plan notification = %#v, want one inProgress step", plan)
	}
}

type nestedSink struct {
	starts    []Item
	completes []Item
	notes     []string
	children  []ChildSpawnedNotification
}

func (s *nestedSink) turn(string, Turn) {}

func (s *nestedSink) item(method, _ string, it *Item) {
	switch method {
	case methodItemStarted:
		s.starts = append(s.starts, *it)
	case methodItemCompleted:
		s.completes = append(s.completes, *it)
	}
}

func (s *nestedSink) notify(method string, build func() writer) {
	s.notes = append(s.notes, method)
	if method == "dalgona/thread/childSpawned" {
		if n, ok := build().(ChildSpawnedNotification); ok {
			s.children = append(s.children, n)
		}
	}
}

func startsOf(items []Item, callID string) []*Item {
	var found []*Item
	for i := range items {
		if items[i].ID == callID {
			found = append(found, &items[i])
		}
	}
	return found
}

func requireOnlyNestedStart(t *testing.T, started *Item) {
	t.Helper()
	if started.Tool != "task" || started.Type != kindDynamic || started.Status != statusInProgress {
		t.Fatalf("nested start = %+v, want task dynamicToolCall inProgress", started)
	}
}

func requireNestedSpawn(t *testing.T, sink *nestedSink, tr *tracker) {
	t.Helper()
	if len(sink.children) != 1 || sink.children[0].ChildID != "child-1" {
		t.Fatalf("childSpawned notifications = %+v, want child-1", sink.children)
	}
	working := tr.items["run/1"]
	if working == nil || working.Tool != "task" || working.Status != statusInProgress {
		t.Fatalf("child working item = %+v, want open task", working)
	}
	if tr.items["run"] == nil {
		t.Fatal("parent eval item closed before its child completed")
	}
}

func requireNestedCompletion(t *testing.T, sink *nestedSink) {
	t.Helper()
	done := startsOf(sink.completes, "run/1")
	if len(done) != 1 || done[0].Tool != "task" || done[0].Status != statusCompleted || done[0].Output != "There are 5 files." {
		t.Fatalf("nested completion = %+v, want completed task with output", done)
	}
}

func requireDeclinedNestedResult(t *testing.T, sink *nestedSink) {
	t.Helper()
	if declined := startsOf(sink.starts, "run/2"); len(declined) != 0 {
		t.Fatalf("declined nested call started %+v", declined[0])
	}
	failed := startsOf(sink.completes, "run/2")
	if len(failed) != 1 || failed[0].Status != statusFailed {
		t.Fatalf("declined nested result = %+v, want one failed completion", failed)
	}
}

func TestNestedApprovalOpensFullItemBeforeChildSpawn(t *testing.T) {
	t.Parallel()
	sink := &nestedSink{}
	tr := newTracker("t1", nil, func(string) string { return "" }, nil, slog.New(slog.DiscardHandler))
	tr.sink = sink
	tr.apply(1, event.TurnStarted{TurnID: "turn-1"})
	tr.apply(2, event.ToolStarted{CallID: "run", TurnID: "turn-1", Tool: "eval", Class: "script", Args: jsontext.Value(`{"code":"1"}`)})
	tr.apply(3, event.ApprovalRequested{RequestID: "req-1", CallID: "run/1", Class: "agent", Summary: "helper task: count files"})
	if thin := startsOf(sink.starts, "run/1"); len(thin) != 0 {
		t.Fatalf("approval without call metadata started %+v", thin[0])
	}
	tr.apply(4, event.ToolStarted{CallID: "run/1", TurnID: "turn-1", Tool: "task", Class: "agent", Args: jsontext.Value(`{"prompt":"helper task: count files"}`)})
	tr.apply(5, event.ChildSpawned{CallID: "run/1", ChildID: "child-1", Runner: "local"})
	started := startsOf(sink.starts, "run/1")
	if len(started) != 1 {
		t.Fatalf("nested call started %d times, want once", len(started))
	}
	requireOnlyNestedStart(t, started[0])
	requireNestedSpawn(t, sink, tr)
	tr.apply(6, event.ToolCompleted{CallID: "run/1", OK: true, Output: "There are 5 files."})
	requireNestedCompletion(t, sink)

	tr.apply(7, event.ApprovalRequested{RequestID: "req-2", CallID: "run/2", Class: "agent", Summary: "helper task"})
	tr.apply(8, event.ApprovalResolved{RequestID: "req-2", Decision: "decline", By: "mode:yolo"})
	tr.apply(9, event.ToolCompleted{CallID: "run/2", OK: false})
	requireDeclinedNestedResult(t, sink)
}
