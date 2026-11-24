package daltoki

import (
	"context"
	"fmt"
	"strconv"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dalgona/graph"
)

const (
	dispatchPeriod    = 30 * time.Second
	draftBodyLimit    = 60000
	pullTitleLimit    = 256
	workflowsDir      = ".github/workflows/"
	interruptedRun    = "daltoki: run was interrupted"
	interruptedSend   = "daltoki: send was interrupted"
	triageInstruction = "Triage the GitHub issue below in this checkout. Read the code it concerns. " +
		"Do not change any file. Reply with the comment a maintainer would post: the likely cause, " +
		"the affected files, and the next step. Reply with the comment text only."
	planInstruction = "Investigate the GitHub issue below in this checkout. Do not change any file. " +
		"Name the root cause and the smallest change that fixes it, with the files and functions to edit."
	fixInstruction = "Fix the GitHub issue below in this checkout with the smallest correct change, " +
		"following the plan. Add or update a test that fails without the fix. Do not edit files under " +
		".github/workflows. Finish with a short pull request description of the change."
)

type jobOutcome struct {
	state  JobState
	reason string
	usage  Usage
	draft  *draftRecord
}

func (s *serviceStore) Finish(ctx context.Context, id string, outcome jobOutcome) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	var records []serviceRecord
	if outcome.draft != nil {
		records = append(records, s.draftRecordOf(kindDraftCreated, *outcome.draft))
	}
	usage := outcome.usage
	records = append(records, s.jobStateRecord(id, outcome.state, outcome.reason, &usage))
	return s.commit(ctx, records...)
}

func (s *Service) wake() {
	select {
	case s.wakeup <- struct{}{}:
	default:
	}
}

func (s *Service) budgetLimits() BudgetLimits {
	return BudgetLimits{
		JobsPerDay: s.cfg.JobsPerDay,
		Job:        Usage{Tokens: int64(s.cfg.JobTokenLimit), CostUSDMicros: s.cfg.JobCostLimitUSDMicros},
		Daily:      Usage{Tokens: int64(s.cfg.DailyTokenLimit), CostUSDMicros: s.cfg.DailyCostLimitUSDMicros},
	}
}

func (s *Service) dispatchLoop(ctx context.Context) {
	ticker := time.NewTicker(dispatchPeriod)
	defer ticker.Stop()
	for {
		s.startQueued(ctx)
		if s.publicationOpen() {
			s.publishApproved(ctx)
		}
		select {
		case <-ctx.Done():
			return
		case <-s.wakeup:
		case <-ticker.C:
		}
	}
}

func (s *Service) takeSlot() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed || s.active >= s.cfg.Concurrency {
		return false
	}
	s.active++
	return true
}

func (s *Service) releaseSlot() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.active--
}

func (s *Service) startQueued(ctx context.Context) {
	for ctx.Err() == nil && s.takeSlot() {
		job, started, err := s.store.StartNext(ctx, s.budgetLimits())
		if err != nil {
			s.deps.Logger.Error("daltoki: job dispatch failed", "error", err)
		}
		if err != nil || !started {
			s.releaseSlot()
			return
		}
		s.workers.Add(1)
		go s.runJob(ctx, job)
	}
}

func (s *Service) runJob(ctx context.Context, job Job) {
	defer s.workers.Done()
	defer s.wake()
	defer s.releaseSlot()
	outcome := s.execute(ctx, job)
	if outcome.state != JobUncertain {
		s.discard(ctx, job)
	}
	if err := s.store.Finish(context.WithoutCancel(ctx), job.ID, outcome); err != nil {
		s.deps.Logger.Error("daltoki: job outcome was not recorded", "job", job.ID, "error", err)
	}
}

func (s *Service) settle(ctx context.Context, usage Usage, err error) jobOutcome {
	if ctx.Err() != nil {
		return jobOutcome{state: JobUncertain, reason: interruptedRun, usage: usage}
	}
	return jobOutcome{state: JobFailed, reason: err.Error(), usage: usage}
}

func fixBranch(job string) string {
	return "daltoki/" + job
}

func (s *Service) execute(ctx context.Context, job Job) jobOutcome {
	info, _, err := s.deps.Client.Repository(ctx, job.Repository)
	if err != nil {
		return s.settle(ctx, job.Usage, fmt.Errorf("daltoki: read repository: %w", err))
	}
	request := WorktreeRequest{JobID: job.ID, Repository: job.Repository, Base: info.DefaultBranch}
	if job.Kind == JobFix {
		request.Branch = fixBranch(job.ID)
	}
	tree, err := s.deps.Worktrees.Prepare(ctx, request)
	if err != nil {
		return s.settle(ctx, job.Usage, err)
	}
	result, err := s.deps.Graphs.Run(ctx, GraphRun{
		JobID: job.ID, Graph: jobGraph(job, s.cfg.Profile), Workspace: tree.Dir, Model: s.cfg.Model,
		Limit: s.budgetLimits().Job, Spent: job.Usage,
		Thread: func(ctx context.Context, node, thread string) error {
			return s.store.RecordThread(ctx, job.ID, node, thread)
		},
		Changes: s.deps.Worktrees.Changes,
	})
	if err != nil {
		return s.settle(ctx, result.Usage, err)
	}
	if job.Kind == JobTriage {
		return s.triaged(job, result)
	}
	return s.fixed(ctx, job, tree, result)
}

func issueBrief(job Job) string {
	return "\n\nIssue: " + repositoryName(job.Repository) + "#" + strconv.FormatInt(job.Issue, 10) +
		"\nTitle: " + job.Title + "\n\n" + job.Body
}

func jobGraph(job Job, profile string) graph.Graph {
	brief := issueBrief(job)
	if job.Kind == JobTriage {
		return graph.Graph{Nodes: []graph.Node{{ID: "triage", Prompt: triageInstruction + brief}}}
	}
	return graph.Graph{Nodes: []graph.Node{
		{ID: "plan", Prompt: planInstruction + brief},
		{ID: "fix", Needs: []string{"plan"}, Profile: profile, Prompt: fixInstruction + brief, Writes: []string{"."}},
	}}
}

func (s *Service) discard(ctx context.Context, job Job) {
	if err := s.deps.Worktrees.Remove(ctx, job.Repository, job.ID); err != nil {
		s.deps.Logger.Warn("daltoki: worktree was not removed", "job", job.ID, "error", err)
	}
}

func (s *Service) triaged(job Job, result GraphResult) jobOutcome {
	reply := boundedText(strings.TrimSpace(result.Final), draftBodyLimit)
	if reply == "" {
		return jobOutcome{state: JobFailed, reason: "daltoki: triage produced no reply", usage: result.Usage}
	}
	draft := &draftRecord{ID: draftID(job.ID), JobID: job.ID, Kind: DraftComment, Body: reply}
	return jobOutcome{state: JobWaitingApproval, usage: result.Usage, draft: draft}
}

func workflowChange(changes map[string]string) (string, bool) {
	for path := range changes {
		if strings.HasPrefix(strings.ToLower(path), workflowsDir) {
			return path, true
		}
	}
	return "", false
}

func (s *Service) fixed(ctx context.Context, job Job, tree Worktree, result GraphResult) jobOutcome {
	changes, err := s.deps.Worktrees.Changes(ctx, tree.Dir)
	if err != nil {
		return s.settle(ctx, result.Usage, err)
	}
	if len(changes) == 0 {
		return jobOutcome{state: JobCompleted, reason: "daltoki: fix produced no change", usage: result.Usage}
	}
	if path, touched := workflowChange(changes); touched {
		return jobOutcome{state: JobFailed, reason: "daltoki: fix changes workflow file " + path, usage: result.Usage}
	}
	issue := "#" + strconv.FormatInt(job.Issue, 10)
	commit, err := s.deps.Worktrees.Commit(ctx, tree.Dir, "Fix "+issue+": "+job.Title)
	if err != nil {
		return s.settle(ctx, result.Usage, err)
	}
	summary := boundedText(strings.TrimSpace(result.Final), draftBodyLimit)
	draft := &draftRecord{
		ID: draftID(job.ID), JobID: job.ID, Kind: DraftPullRequest,
		Title: boundedText("Fix "+issue+": "+job.Title, pullTitleLimit),
		Body:  strings.TrimSpace(summary + "\n\nFixes " + issue),
		Head:  tree.Branch, Base: tree.Base, Commit: commit,
	}
	return jobOutcome{state: JobWaitingApproval, usage: result.Usage, draft: draft}
}

func (s *Service) recoverState(ctx context.Context) error {
	for _, job := range s.store.Jobs() {
		if err := s.recoverJob(ctx, job); err != nil {
			return err
		}
	}
	for _, draft := range s.store.Drafts() {
		if draft.State != DraftSending {
			continue
		}
		if err := s.store.MarkUncertain(ctx, draft.ID, interruptedSend); err != nil {
			return err
		}
	}
	for _, draft := range s.store.Drafts() {
		if draft.State != DraftUncertain {
			continue
		}
		if _, err := s.reconcile(ctx, draft); err != nil {
			s.deps.Logger.Warn("daltoki: uncertain draft is not reconciled yet", "draft", draft.ID, "error", err)
		}
	}
	return nil
}

func (s *Service) recoverJob(ctx context.Context, job Job) error {
	if job.State == JobRunning {
		usage := s.reclaimJobThreads(ctx, job)
		if err := s.store.MoveJob(ctx, job.ID, JobUncertain, interruptedRun, &usage); err != nil {
			return err
		}
		job.State = JobUncertain
	}
	if job.State != JobUncertain {
		return nil
	}
	return s.store.MoveJob(ctx, job.ID, JobQueued, "", nil)
}

type threadReclaimer interface {
	ReclaimThread(ctx context.Context, threadID string) (Usage, error)
}

func (s *Service) reclaimJobThreads(ctx context.Context, job Job) Usage {
	usage := job.Usage
	reclaimer := s.reclaimer
	if reclaimer == nil && s.host != nil {
		reclaimer = hostReclaimer{host: s.host}
	}
	if reclaimer == nil {
		return usage
	}
	for _, recorded := range job.Threads {
		reclaimed, err := reclaimer.ReclaimThread(ctx, recorded.Thread)
		if err != nil {
			s.deps.Logger.Warn("daltoki: recovered thread was not reconciled", "job", job.ID, "thread", recorded.Thread, "error", err)
			continue
		}
		usage = usage.plus(reclaimed)
	}
	return usage
}

type hostReclaimer struct {
	host *agent.Host
}

func (r hostReclaimer) ReclaimThread(ctx context.Context, threadID string) (Usage, error) {
	thread, err := r.host.Resume(ctx, threadID)
	if err != nil {
		return Usage{}, err
	}
	defer func() { _ = thread.Close() }()
	thread.Interrupt()
	records, state, err := thread.Records()
	if err != nil {
		return Usage{}, err
	}
	price, priced := r.host.PriceOf(state.Model)
	if !priced {
		return Usage{}, fmt.Errorf("model %s has no price, so its cost budget cannot be enforced", state.Model)
	}
	return threadUsage(records, price), nil
}

func threadUsage(records []event.Seqd, price model.Price) Usage {
	var used Usage
	for i := range records {
		message, ok := records[i].Event.(event.AssistantMessage)
		if !ok {
			continue
		}
		used = used.plus(usageOf(price, message.Usage))
	}
	return used
}
