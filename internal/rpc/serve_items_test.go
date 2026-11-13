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
