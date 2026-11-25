package daltoki

import (
	"context"
	"errors"
	"fmt"
	"maps"
	"slices"
	"strings"
	"sync"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dalgona/graph"
)

const needOutputLimit = 16384

var (
	errSchedulerClosed = errors.New("daltoki: graph scheduler is closed")
	errJobBudget       = errors.New("daltoki: job budget exhausted")
	errHostDetached    = errors.New("daltoki: agent host is not attached")
)

type GraphScheduler interface {
	Attach(host *agent.Host)
	Run(ctx context.Context, run GraphRun) (GraphResult, error)
	CancelAll(ctx context.Context) error
}

type GraphRun struct {
	JobID     string
	Graph     graph.Graph
	Workspace string
	Model     string
	Limit     Usage
	Spent     Usage
	Thread    func(ctx context.Context, node, thread string) error
	Changes   func(ctx context.Context, dir string) (map[string]string, error)
}

type GraphResult struct {
	Outputs map[string]string
	Final   string
	Usage   Usage
}

type NodeRun struct {
	Node      graph.Node
	Prompt    string
	Workspace string
	Model     string
	Remaining Usage
	Started   func(ctx context.Context, thread string) error
}

type NodeResult struct {
	Text  string
	Usage Usage
}

type NodeRunner interface {
	RunNode(ctx context.Context, host *agent.Host, run NodeRun) (NodeResult, error)
}

type Scheduler struct {
	nodes  NodeRunner
	mu     sync.Mutex
	host   *agent.Host
	runs   map[string]context.CancelFunc
	closed bool
}

func NewScheduler(nodes NodeRunner) *Scheduler {
	return &Scheduler{nodes: nodes, runs: map[string]context.CancelFunc{}}
}

func (s *Scheduler) Attach(host *agent.Host) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.host = host
}

func (s *Scheduler) CancelAll(ctx context.Context) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.closed = true
	for _, cancel := range s.runs {
		cancel()
	}
	return ctx.Err()
}

func (s *Scheduler) begin(ctx context.Context, jobID string) (context.Context, *agent.Host, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return nil, nil, errSchedulerClosed
	}
	if _, running := s.runs[jobID]; running {
		return nil, nil, fmt.Errorf("daltoki: job %s already runs a graph", jobID)
	}
	runCtx, cancel := context.WithCancel(ctx)
	s.runs[jobID] = cancel
	return runCtx, s.host, nil
}

func (s *Scheduler) end(jobID string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if cancel, ok := s.runs[jobID]; ok {
		cancel()
		delete(s.runs, jobID)
	}
}

func (s *Scheduler) Run(ctx context.Context, run GraphRun) (GraphResult, error) {
	result := GraphResult{Outputs: map[string]string{}, Usage: run.Spent}
	if err := graph.Validate(run.Graph); err != nil {
		return result, err
	}
	runCtx, host, err := s.begin(ctx, run.JobID)
	if err != nil {
		return result, err
	}
	defer s.end(run.JobID)
	for _, node := range executionOrder(run.Graph.Nodes) {
		if err := s.runNode(runCtx, host, run, node, &result); err != nil {
			return result, err
		}
	}
	return result, nil
}

func executionOrder(nodes []graph.Node) []graph.Node {
	done := make(map[string]bool, len(nodes))
	order := make([]graph.Node, 0, len(nodes))
	for len(order) < len(nodes) {
		for _, node := range nodes {
			if done[node.ID] || !allDone(node.Needs, done) {
				continue
			}
			done[node.ID] = true
			order = append(order, node)
			break
		}
	}
	return order
}

func allDone(needs []string, done map[string]bool) bool {
	for _, need := range needs {
		if !done[need] {
			return false
		}
	}
	return true
}

func composePrompt(node graph.Node, outputs map[string]string) string {
	var prompt strings.Builder
	prompt.WriteString(node.Prompt)
	for _, need := range node.Needs {
		prompt.WriteString("\n\nResult of " + need + ":\n")
		prompt.WriteString(boundedText(outputs[need], needOutputLimit))
	}
	return prompt.String()
}

func (s *Scheduler) runNode(ctx context.Context, host *agent.Host, run GraphRun, node graph.Node, result *GraphResult) error {
	remaining := Usage{Tokens: run.Limit.Tokens - result.Usage.Tokens, CostUSDMicros: run.Limit.CostUSDMicros - result.Usage.CostUSDMicros}
	if remaining.Tokens <= 0 || remaining.CostUSDMicros <= 0 {
		return errJobBudget
	}
	before, err := run.Changes(ctx, run.Workspace)
	if err != nil {
		return err
	}
	out, err := s.nodes.RunNode(ctx, host, NodeRun{
		Node: node, Prompt: composePrompt(node, result.Outputs),
		Workspace: run.Workspace, Model: run.Model, Remaining: remaining,
		Started: func(ctx context.Context, thread string) error { return run.Thread(ctx, node.ID, thread) },
	})
	result.Usage = result.Usage.plus(out.Usage)
	if err != nil {
		return fmt.Errorf("daltoki: node %s: %w", node.ID, err)
	}
	if !result.Usage.within(run.Limit) {
		return errJobBudget
	}
	after, err := run.Changes(ctx, run.Workspace)
	if err != nil {
		return err
	}
	if path, outside := outsideClaims(node.Writes, before, after); outside {
		return fmt.Errorf("graph: node %q wrote outside its claims at %q", node.ID, path)
	}
	result.Outputs[node.ID], result.Final = out.Text, out.Text
	return nil
}

func outsideClaims(writes []string, before, after map[string]string) (string, bool) {
	changed := map[string]bool{}
	for path, digest := range after {
		if before[path] != digest {
			changed[path] = true
		}
	}
	for path := range before {
		if _, kept := after[path]; !kept {
			changed[path] = true
		}
	}
	for _, path := range slices.Sorted(maps.Keys(changed)) {
		if !claimed(writes, path) {
			return path, true
		}
	}
	return "", false
}

func claimed(writes []string, path string) bool {
	for _, claim := range writes {
		if claim == "." || path == claim || strings.HasPrefix(path, claim+"/") {
			return true
		}
	}
	return false
}

type HostNodes struct{}

func nodeThreadSpec(run NodeRun) agent.ThreadSpec {
	return agent.ThreadSpec{
		Cwd: run.Workspace, Model: run.Model, Approval: config.ApprovalAuto, Sandbox: config.SandboxWorkspaceWrite,
		ReadOnly: len(run.Node.Writes) == 0, Profile: run.Node.Profile,
	}
}

func (HostNodes) RunNode(ctx context.Context, host *agent.Host, run NodeRun) (NodeResult, error) {
	if host == nil {
		return NodeResult{}, errHostDetached
	}
	thread, err := host.Start(ctx, nodeThreadSpec(run))
	if err != nil {
		return NodeResult{}, fmt.Errorf("start thread: %w", err)
	}
	defer func() { _ = thread.Close() }()
	if startErr := run.Started(ctx, thread.ID()); startErr != nil {
		return NodeResult{}, startErr
	}
	_, state, err := thread.Records()
	if err != nil {
		return NodeResult{}, fmt.Errorf("read thread: %w", err)
	}
	price, priced := host.PriceOf(state.Model)
	if !priced {
		return NodeResult{}, fmt.Errorf("model %s has no price, so its cost budget cannot be enforced", state.Model)
	}
	meter := nodeMeter{price: price, limit: run.Remaining}
	runErr := meter.await(ctx, thread, run.Prompt)
	text, usage, readErr := nodeOutcome(thread, price)
	return NodeResult{Text: text, Usage: usage}, errors.Join(runErr, readErr)
}

type nodeMeter struct {
	price model.Price
	limit Usage
	used  Usage
}

func usageOf(price model.Price, usage event.Usage) Usage {
	counts := model.TokenCounts{Input: usage.Input, Output: usage.Output, CacheRead: usage.CacheRead, CacheWrite: usage.CacheWrite}
	tokens := int64(usage.Input) + int64(usage.Output) + int64(usage.CacheRead) + int64(usage.CacheWrite)
	return Usage{Tokens: tokens, CostUSDMicros: price.CostMicros(counts)}
}

func (m *nodeMeter) await(ctx context.Context, thread *agent.Thread, prompt string) error {
	updates := thread.Subscribe()
	defer func() { thread.Unsubscribe(updates) }()
	if err := thread.Submit(ctx, agent.Input{Kind: agent.InputPrompt, Text: prompt}); err != nil {
		return fmt.Errorf("submit prompt: %w", err)
	}
	for {
		status, err := thread.Status()
		if err != nil || (!status.Busy && status.Turn == "") {
			return err
		}
		select {
		case update := <-updates:
			if err := m.observe(thread, updates, update); err != nil {
				thread.Interrupt()
				return err
			}
		case <-ctx.Done():
			thread.Interrupt()
			return context.Cause(ctx)
		}
	}
}

func (m *nodeMeter) observe(thread *agent.Thread, updates <-chan agent.Update, update agent.Update) error {
	if update.Kind == agent.UpdateApproval {
		return thread.CannotAnswerApproval(update.Approval.RequestID, updates)
	}
	if update.Kind == agent.UpdateQuestion {
		return thread.CannotAnswerQuestion(update.Question.RequestID, updates)
	}
	message, ok := update.Event.(event.AssistantMessage)
	if update.Kind != agent.UpdateEvent || !ok {
		return nil
	}
	m.used = m.used.plus(usageOf(m.price, message.Usage))
	if !m.used.within(m.limit) {
		return errJobBudget
	}
	return nil
}

func nodeOutcome(thread *agent.Thread, price model.Price) (string, Usage, error) {
	records, _, err := thread.Records()
	if err != nil {
		return "", Usage{}, fmt.Errorf("read thread: %w", err)
	}
	var used Usage
	var last event.AssistantMessage
	for i := range records {
		message, ok := records[i].Event.(event.AssistantMessage)
		if !ok {
			continue
		}
		used = used.plus(usageOf(price, message.Usage))
		if message.Text != "" || message.TextBlob != nil {
			last = message
		}
	}
	text, err := thread.LoadText(last.Text, last.TextBlob)
	return text, used, errors.Join(turnFailure(records), err)
}

func turnFailure(records []event.Seqd) error {
	for i := range records {
		completed, ok := records[i].Event.(event.TurnCompleted)
		if !ok || completed.Status == "completed" {
			continue
		}
		if completed.Error == "" {
			return fmt.Errorf("daltoki: node turn %s", completed.Status)
		}
		return fmt.Errorf("daltoki: node turn %s: %s", completed.Status, completed.Error)
	}
	return nil
}
