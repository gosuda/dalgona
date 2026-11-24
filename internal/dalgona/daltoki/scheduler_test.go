package daltoki

import (
	"context"
	"errors"
	"maps"
	"strings"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dalgona/graph"
)

type fakeWorkspace struct {
	mu    sync.Mutex
	files map[string]string
}

func (w *fakeWorkspace) changes(context.Context, string) (map[string]string, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return maps.Clone(w.files), nil
}

func (w *fakeWorkspace) write(path, content string) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.files[path] = content
}

type scriptedNodes struct {
	mu      sync.Mutex
	space   *fakeWorkspace
	writes  map[string][]string
	usage   Usage
	block   bool
	order   []string
	prompts map[string]string
}

func (n *scriptedNodes) RunNode(ctx context.Context, _ *agent.Host, run NodeRun) (NodeResult, error) {
	n.mu.Lock()
	n.order = append(n.order, run.Node.ID)
	n.prompts[run.Node.ID] = run.Prompt
	n.mu.Unlock()
	if err := run.Started(ctx, "thread-"+run.Node.ID); err != nil {
		return NodeResult{}, err
	}
	for _, path := range n.writes[run.Node.ID] {
		n.space.write(path, run.Node.ID)
	}
	if n.block {
		<-ctx.Done()
		return NodeResult{}, ctx.Err()
	}
	return NodeResult{Text: "out-" + run.Node.ID, Usage: n.usage}, nil
}

func newScriptedNodes() *scriptedNodes {
	return &scriptedNodes{space: &fakeWorkspace{files: map[string]string{}}, writes: map[string][]string{}, prompts: map[string]string{}}
}

func graphRun(nodes *scriptedNodes, value graph.Graph, threads *[]string) GraphRun {
	return GraphRun{
		JobID: "j-1", Graph: value, Workspace: "/work", Model: "model",
		Limit:   Usage{Tokens: 1000, CostUSDMicros: 1000},
		Changes: nodes.space.changes,
		Thread: func(_ context.Context, node, thread string) error {
			*threads = append(*threads, node+"="+thread)
			return nil
		},
	}
}

func TestSchedulerRunsNeedsBeforeDependents(t *testing.T) {
	nodes := newScriptedNodes()
	var threads []string
	value := graph.Graph{Nodes: []graph.Node{
		{ID: "report", Needs: []string{"scan"}, Prompt: "Write the report."},
		{ID: "scan", Prompt: "Scan the code."},
	}}
	result, err := NewScheduler(nodes).Run(context.Background(), graphRun(nodes, value, &threads))
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	if strings.Join(nodes.order, ",") != "scan,report" {
		t.Fatalf("order = %v", nodes.order)
	}
	if !strings.Contains(nodes.prompts["report"], "Result of scan:\nout-scan") {
		t.Fatalf("report prompt = %q", nodes.prompts["report"])
	}
	if result.Final != "out-report" || strings.Join(threads, ",") != "scan=thread-scan,report=thread-report" {
		t.Fatalf("final=%q threads=%v", result.Final, threads)
	}
}

func TestSchedulerRefusesWritesOutsideClaims(t *testing.T) {
	cases := []struct {
		name   string
		claims []string
		writes []string
		want   string
	}{
		{"read-only node writes", nil, []string{"main.go"}, `graph: node "work" wrote outside its claims at "main.go"`},
		{"claimed directory", []string{"src"}, []string{"src/a.go"}, ""},
		{"sibling with claim prefix", []string{"src"}, []string{"srcx/a.go"}, `graph: node "work" wrote outside its claims at "srcx/a.go"`},
		{"whole workspace", []string{"."}, []string{"a.go", "docs/b.md"}, ""},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			nodes := newScriptedNodes()
			nodes.writes["work"] = tc.writes
			var threads []string
			value := graph.Graph{Nodes: []graph.Node{{ID: "work", Prompt: "Work.", Writes: tc.claims}}}
			_, err := NewScheduler(nodes).Run(context.Background(), graphRun(nodes, value, &threads))
			got := ""
			if err != nil {
				got = err.Error()
			}
			if got != tc.want {
				t.Fatalf("err = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestSchedulerStopsAtJobBudget(t *testing.T) {
	nodes := newScriptedNodes()
	nodes.usage = Usage{Tokens: 400, CostUSDMicros: 1}
	var threads []string
	value := graph.Graph{Nodes: []graph.Node{
		{ID: "first", Prompt: "One."},
		{ID: "second", Needs: []string{"first"}, Prompt: "Two."},
	}}
	run := graphRun(nodes, value, &threads)
	run.Spent = Usage{Tokens: 700}
	result, err := NewScheduler(nodes).Run(context.Background(), run)
	if !errors.Is(err, errJobBudget) {
		t.Fatalf("err = %v", err)
	}
	if len(nodes.order) != 1 || result.Usage.Tokens != 1100 {
		t.Fatalf("order=%v usage=%+v", nodes.order, result.Usage)
	}
}

func TestSchedulerCancelAllStopsRunsAndRefusesNewOnes(t *testing.T) {
	nodes := newScriptedNodes()
	nodes.block = true
	scheduler := NewScheduler(nodes)
	var threads []string
	value := graph.Graph{Nodes: []graph.Node{{ID: "work", Prompt: "Work."}}}
	done := make(chan error, 1)
	go func() {
		_, err := scheduler.Run(context.Background(), graphRun(nodes, value, &threads))
		done <- err
	}()
	waitFor(t, "node start", func() bool {
		nodes.mu.Lock()
		defer nodes.mu.Unlock()
		return len(nodes.order) == 1
	})
	if err := scheduler.CancelAll(context.Background()); err != nil {
		t.Fatalf("CancelAll: %v", err)
	}
	if err := <-done; !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled run = %v", err)
	}
	if _, err := scheduler.Run(context.Background(), graphRun(nodes, value, &threads)); !errors.Is(err, errSchedulerClosed) {
		t.Fatalf("run after CancelAll = %v", err)
	}
}

func TestTurnFailureRejectsFailedTurn(t *testing.T) {
	failed := []event.Seqd{{Event: event.TurnCompleted{TurnID: "t", Status: "failed", Error: "provider blew up"}}}
	if err := turnFailure(failed); err == nil || !strings.Contains(err.Error(), "daltoki: node turn failed") {
		t.Fatalf("failed turn = %v", err)
	}
	interrupted := []event.Seqd{{Event: event.TurnCompleted{TurnID: "t", Status: "interrupted"}}}
	if err := turnFailure(interrupted); err == nil || !strings.Contains(err.Error(), "interrupted") {
		t.Fatalf("interrupted turn = %v", err)
	}
	completed := []event.Seqd{
		{Event: event.AssistantMessage{TurnID: "t", Text: "done"}},
		{Event: event.TurnCompleted{TurnID: "t", Status: "completed"}},
	}
	if err := turnFailure(completed); err != nil {
		t.Fatalf("completed turn = %v", err)
	}
	if err := turnFailure(nil); err != nil {
		t.Fatalf("empty records = %v", err)
	}
}
