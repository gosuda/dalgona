package daltoki

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"slices"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dalgona/github"
)

const (
	daltokiLabel      = "daltoki"
	failureMarker     = "[dalgona-failure:v1:"
	issueTitleLimit   = 1024
	issueBodyLimit    = 32768
	issueHistoryPages = 10
)

type JobKind string

const (
	JobTriage JobKind = "triage"
	JobFix    JobKind = "fix"
)

type JobState string

const (
	JobQueued          JobState = "queued"
	JobRunning         JobState = "running"
	JobWaitingApproval JobState = "waiting-approval"
	JobBlocked         JobState = "blocked"
	JobCompleted       JobState = "completed"
	JobFailed          JobState = "failed"
	JobUncertain       JobState = "uncertain"
)

var jobTransitions = map[JobState][]JobState{
	JobQueued:          {JobRunning, JobBlocked},
	JobRunning:         {JobWaitingApproval, JobCompleted, JobFailed, JobUncertain},
	JobUncertain:       {JobQueued},
	JobWaitingApproval: {JobCompleted, JobFailed},
}

func (s JobState) terminal() bool {
	return s == JobBlocked || s == JobCompleted || s == JobFailed
}

type Usage struct {
	Tokens        int64 `json:"tokens"`
	CostUSDMicros int64 `json:"costUsdMicros"`
}

func (u Usage) plus(other Usage) Usage {
	return Usage{Tokens: u.Tokens + other.Tokens, CostUSDMicros: u.CostUSDMicros + other.CostUSDMicros}
}

func (u Usage) within(limit Usage) bool {
	return u.Tokens <= limit.Tokens && u.CostUSDMicros <= limit.CostUSDMicros
}

type JobThread struct {
	Node   string
	Thread string
}

type JobPush struct {
	Branch string
	Commit string
}

type Job struct {
	ID         string
	Kind       JobKind
	Repository github.Repository
	Issue      int64
	Title      string
	Body       string
	Actor      string
	State      JobState
	Reason     string
	Usage      Usage
	Threads    []JobThread
	Push       *JobPush
}

type jobRecord struct {
	ID         string   `json:"id"`
	Kind       JobKind  `json:"kind,omitempty"`
	Repository string   `json:"repository,omitempty"`
	Issue      int64    `json:"issue,omitempty"`
	Title      string   `json:"title,omitempty"`
	Body       string   `json:"body,omitempty"`
	Actor      string   `json:"actor,omitempty"`
	Trigger    string   `json:"trigger,omitempty"`
	State      JobState `json:"state,omitempty"`
	Reason     string   `json:"reason,omitempty"`
	Usage      *Usage   `json:"usage,omitempty"`
	Node       string   `json:"node,omitempty"`
	Thread     string   `json:"thread,omitempty"`
	Branch     string   `json:"branch,omitempty"`
	Commit     string   `json:"commit,omitempty"`
}

func repositoryName(repo github.Repository) string {
	return repo.Owner + "/" + repo.Name
}

func parseRepositoryName(name string) (github.Repository, bool) {
	owner, repo, ok := strings.Cut(strings.ToLower(name), "/")
	if !ok || !validRepositoryPart(owner) || !validRepositoryPart(repo) {
		return github.Repository{}, false
	}
	return github.Repository{Owner: owner, Name: repo}, true
}

func issueKey(kind JobKind, repository string, issue int64) string {
	return string(kind) + "|" + repository + "|" + strconv.FormatInt(issue, 10)
}

func jobID(trigger string) string {
	sum := sha256.Sum256([]byte(trigger))
	return "j-" + hex.EncodeToString(sum[:8])
}

var jobChecks = map[string]func(job *Job, record jobRecord) bool{
	kindJobState:  jobStateAllowed,
	kindJobThread: jobThreadAllowed,
	kindJobPush:   jobPushAllowed,
}

func jobStateAllowed(job *Job, record jobRecord) bool {
	return slices.Contains(jobTransitions[job.State], record.State) && validUsage(record.Usage)
}

func jobThreadAllowed(job *Job, record jobRecord) bool {
	return job.State == JobRunning && record.Node != "" && record.Thread != ""
}

func jobPushAllowed(job *Job, record jobRecord) bool {
	return job.Kind == JobFix && job.State == JobWaitingApproval && record.Branch != "" && record.Commit != ""
}

func (s *serviceStore) checkJob(kind string, record jobRecord) error {
	if kind == kindJobCreated {
		return s.checkJobCreated(record)
	}
	job, ok := s.jobs[record.ID]
	if !ok || !jobChecks[kind](job, record) {
		return errJournalDamaged
	}
	return nil
}

func (s *serviceStore) checkJobCreated(record jobRecord) error {
	_, exists := s.jobs[record.ID]
	_, repoOK := parseRepositoryName(record.Repository)
	kindOK := record.Kind == JobTriage || record.Kind == JobFix
	if exists || !repoOK || !kindOK || record.ID == "" || record.Issue <= 0 || record.State != JobQueued {
		return errJournalDamaged
	}
	return nil
}

func validUsage(usage *Usage) bool {
	return usage == nil || (usage.Tokens >= 0 && usage.CostUSDMicros >= 0)
}

func (s *serviceStore) applyJob(kind string, record jobRecord) {
	if kind == kindJobCreated {
		repo, _ := parseRepositoryName(record.Repository)
		s.jobs[record.ID] = &Job{
			ID: record.ID, Kind: record.Kind, Repository: repo, Issue: record.Issue,
			Title: record.Title, Body: record.Body, Actor: record.Actor, State: record.State,
		}
		s.jobOrder = append(s.jobOrder, record.ID)
		s.issueJobs[issueKey(record.Kind, record.Repository, record.Issue)] = record.ID
		return
	}
	job := s.jobs[record.ID]
	switch kind {
	case kindJobState:
		job.State, job.Reason = record.State, record.Reason
		if record.Usage != nil {
			job.Usage = *record.Usage
		}
	case kindJobThread:
		job.Threads = append(job.Threads, JobThread{Node: record.Node, Thread: record.Thread})
	case kindJobPush:
		job.Push = &JobPush{Branch: record.Branch, Commit: record.Commit}
	}
}

func (s *serviceStore) issueBusy(kind JobKind, repository string, issue int64) bool {
	id, ok := s.issueJobs[issueKey(kind, repository, issue)]
	if !ok {
		return false
	}
	return kind == JobTriage || !s.jobs[id].State.terminal()
}

func (s *serviceStore) IssueBusy(kind JobKind, repository string, issue int64) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.issueBusy(kind, repository, issue)
}

func (s *serviceStore) Jobs() []Job {
	s.mu.Lock()
	defer s.mu.Unlock()
	jobs := make([]Job, 0, len(s.jobOrder))
	for _, id := range s.jobOrder {
		jobs = append(jobs, s.jobs[id].snapshot())
	}
	return jobs
}

func (j *Job) snapshot() Job {
	copied := *j
	copied.Threads = slices.Clone(j.Threads)
	if j.Push != nil {
		push := *j.Push
		copied.Push = &push
	}
	return copied
}

func (s *serviceStore) Job(id string) (Job, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	job, ok := s.jobs[id]
	if !ok {
		return Job{}, false
	}
	return job.snapshot(), true
}

func (s *serviceStore) jobStateRecord(id string, state JobState, reason string, usage *Usage) serviceRecord {
	record := s.record(kindJobState, s.clock().UTC())
	record.Job = &jobRecord{ID: id, State: state, Reason: reason, Usage: usage}
	return record
}

func (s *serviceStore) MoveJob(ctx context.Context, id string, state JobState, reason string, usage *Usage) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.commit(ctx, s.jobStateRecord(id, state, reason, usage))
}

func (s *serviceStore) RecordThread(ctx context.Context, id, node, thread string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	record := s.record(kindJobThread, s.clock().UTC())
	record.Job = &jobRecord{ID: id, Node: node, Thread: thread}
	return s.commit(ctx, record)
}

type trigger struct {
	kind  JobKind
	repo  github.Repository
	issue int64
	title string
	body  string
	actor string
}

func triageable(actorType, title string, createdAt, since time.Time) bool {
	if createdAt.Before(since) {
		return false
	}
	return actorType != "Bot" || strings.HasPrefix(title, failureMarker)
}

func observationKind(obs Observation, since time.Time) JobKind {
	switch {
	case obs.Kind == "issue" && obs.State == "open" && triageable(obs.ActorType, obs.Title, obs.CreatedAt, since):
		return JobTriage
	case obs.Kind == "event" && obs.Action == "labeled" && slices.Contains(obs.Labels, daltokiLabel):
		return JobFix
	}
	return ""
}

func observationTrigger(obs Observation, since time.Time) *trigger {
	kind := observationKind(obs, since)
	if obs.Ignored || obs.IssueNumber <= 0 || kind == "" {
		return nil
	}
	return &trigger{kind: kind, repo: obs.Repository, issue: obs.IssueNumber, title: obs.Title, body: obs.Body, actor: obs.Actor}
}

type webhookActor struct {
	Login string `json:"login"`
	Type  string `json:"type"`
}

type webhookIssue struct {
	Action string `json:"action"`
	Issue  struct {
		Number      int64          `json:"number"`
		Title       string         `json:"title"`
		Body        string         `json:"body"`
		User        webhookActor   `json:"user"`
		PullRequest jsontext.Value `json:"pull_request"`
		CreatedAt   time.Time      `json:"created_at"`
	} `json:"issue"`
	Label *struct {
		Name string `json:"name"`
	} `json:"label"`
	Sender     webhookActor `json:"sender"`
	Repository struct {
		FullName string `json:"full_name"`
	} `json:"repository"`
}

func (p webhookIssue) kind(since time.Time) JobKind {
	switch {
	case p.Action == "opened" && triageable(p.Issue.User.Type, p.Issue.Title, p.Issue.CreatedAt, since):
		return JobTriage
	case p.Action == "labeled" && p.Label != nil && p.Label.Name == daltokiLabel:
		return JobFix
	}
	return ""
}

func (p webhookIssue) pullRequest() bool {
	return len(p.Issue.PullRequest) > 0 && string(p.Issue.PullRequest) != "null"
}

func envelopeTrigger(envelope Envelope, since time.Time) *trigger {
	var payload webhookIssue
	if envelope.Event != "issues" || json.Unmarshal(envelope.Body, &payload) != nil {
		return nil
	}
	repo, ok := parseRepositoryName(payload.Repository.FullName)
	kind := payload.kind(since)
	if !ok || kind == "" || payload.pullRequest() || payload.Issue.Number <= 0 {
		return nil
	}
	return &trigger{
		kind: kind, repo: repo, issue: payload.Issue.Number,
		title: payload.Issue.Title, body: payload.Issue.Body, actor: payload.Sender.Login,
	}
}

type issueState struct {
	closed   bool
	labeler  string
	labelled bool
	title    string
	body     string
}

func (st *issueState) observe(event github.Event) {
	daltoki := event.Label != nil && event.Label.Name == daltokiLabel
	switch {
	case event.Action == "closed":
		st.closed = true
	case event.Action == "reopened":
		st.closed = false
	case event.Action == "labeled" && daltoki:
		st.labeler = event.Actor.Login
	case event.Action == "unlabeled" && daltoki:
		st.labeler = ""
	}
}

func (st issueState) admits(kind JobKind) bool {
	if st.closed {
		return false
	}
	return kind == JobTriage || (st.labelled && st.labeler != "")
}

func boundedText(text string, limit int) string {
	text = strings.ToValidUTF8(text, "\uFFFD")
	if len(text) <= limit {
		return text
	}
	cut := limit
	for cut > 0 && !utf8.RuneStart(text[cut]) {
		cut--
	}
	return text[:cut]
}

func (t *trigger) record(key string, current issueState) *jobRecord {
	title, body, actor := t.title, t.body, t.actor
	if t.kind == JobFix {
		title, body, actor = current.title, current.body, current.labeler
	}
	return &jobRecord{
		ID:         jobID(key),
		Kind:       t.kind,
		Repository: repositoryName(t.repo),
		Issue:      t.issue,
		Title:      boundedText(title, issueTitleLimit),
		Body:       boundedText(body, issueBodyLimit),
		Actor:      actor,
		Trigger:    key,
		State:      JobQueued,
	}
}

type pollAdmission struct {
	s *Service
}

func (a pollAdmission) LoadCursor(ctx context.Context) (PollCursor, bool, error) {
	return a.s.store.LoadCursor(ctx)
}

func (a pollAdmission) SaveCursor(ctx context.Context, cursor PollCursor) error {
	return a.s.store.SaveCursor(ctx, cursor)
}

func (a pollAdmission) Admit(ctx context.Context, observation Observation) (bool, error) {
	return a.s.admitObservation(ctx, observation)
}

type envelopeStore struct {
	s *Service
}

func (e envelopeStore) Admit(ctx context.Context, envelope Envelope) (bool, error) {
	return e.s.admitEnvelope(ctx, envelope)
}

func (s *Service) admitObservation(ctx context.Context, observation Observation) (bool, error) {
	key := "poll|" + observation.DedupKey()
	if seen, err := s.store.seen(key, ""); err != nil || seen {
		return false, err
	}
	body, err := json.Marshal(observation)
	if err != nil {
		return false, fmt.Errorf("daltoki: encode observation: %w", err)
	}
	job := s.jobFor(ctx, key, observationTrigger(observation, s.store.FirstAt()))
	return s.accept(ctx, deliveryRecord{Source: "poll", Key: key, Observation: body}, job)
}

func (s *Service) admitEnvelope(ctx context.Context, envelope Envelope) (bool, error) {
	key := "relay|" + envelope.DeliveryID
	if seen, err := s.store.seen(key, envelope.BodySHA256); err != nil || seen {
		return false, err
	}
	body, err := json.Marshal(envelope)
	if err != nil {
		return false, fmt.Errorf("daltoki: encode envelope: %w", err)
	}
	job := s.jobFor(ctx, key, envelopeTrigger(envelope, s.store.FirstAt()))
	return s.accept(ctx, deliveryRecord{Source: "relay", Key: key, Digest: envelope.BodySHA256, Envelope: body}, job)
}

func (s *Service) accept(ctx context.Context, delivery deliveryRecord, job *jobRecord) (bool, error) {
	admitted, err := s.store.accept(ctx, delivery, job)
	if admitted && job != nil {
		s.wake()
	}
	return admitted, err
}

func (s *Service) jobFor(ctx context.Context, key string, found *trigger) *jobRecord {
	if found == nil || !slices.Contains(s.cfg.Repositories, found.repo) {
		return nil
	}
	if s.store.IssueBusy(found.kind, repositoryName(found.repo), found.issue) {
		return nil
	}
	current, err := s.currentIssue(ctx, found)
	if err != nil {
		s.deps.Logger.Warn("daltoki: current issue state is unavailable, so no job is admitted",
			"repository", repositoryName(found.repo), "issue", found.issue, "error", err)
		return nil
	}
	if !current.admits(found.kind) {
		return nil
	}
	if found.kind == JobFix && !s.authorized(ctx, found.repo, current.labeler) {
		return nil
	}
	return found.record(key, current)
}

func (s *Service) currentIssue(ctx context.Context, found *trigger) (issueState, error) {
	current, err := s.issueHistory(ctx, found)
	if err != nil || found.kind != JobFix {
		return current, err
	}
	return s.labelledIssue(ctx, found, current)
}

func (s *Service) issueHistory(ctx context.Context, found *trigger) (issueState, error) {
	var current issueState
	request := github.EventsRequest{Repository: found.repo, Number: found.issue}
	for range issueHistoryPages {
		page, meta, err := s.deps.Client.Events(ctx, request)
		if err != nil {
			return issueState{}, fmt.Errorf("daltoki: refetch issue events: %w", err)
		}
		for _, event := range page.Items {
			current.observe(event)
		}
		if meta.NextURL == "" {
			return current, nil
		}
		request.NextURL = meta.NextURL
	}
	return issueState{}, fmt.Errorf("daltoki: issue history exceeds %d pages", issueHistoryPages)
}

func (st *issueState) adopt(items []github.IssueItem, number int64) bool {
	for _, item := range items {
		if item.Number != number || item.PullRequest != nil {
			continue
		}
		st.labelled = true
		st.closed = st.closed || item.State != "open"
		st.title, st.body = item.Title, item.Body
		return true
	}
	return false
}

func (s *Service) labelledIssue(ctx context.Context, found *trigger, current issueState) (issueState, error) {
	request := github.IssuesRequest{Repository: found.repo, Labels: []string{daltokiLabel}}
	for range issueHistoryPages {
		page, meta, err := s.deps.Client.Issues(ctx, request)
		if err != nil {
			return issueState{}, fmt.Errorf("daltoki: refetch labelled issues: %w", err)
		}
		if current.adopt(page.Items, found.issue) || meta.NextURL == "" {
			return current, nil
		}
		request.NextURL = meta.NextURL
	}
	return issueState{}, fmt.Errorf("daltoki: labelled issues exceed %d pages", issueHistoryPages)
}

func (s *Service) authorized(ctx context.Context, repo github.Repository, login string) bool {
	permission, _, err := s.deps.Client.Permission(ctx, repo, login)
	if err != nil {
		s.deps.Logger.Warn("daltoki: label attribution failed, so no fix is authorized",
			"repository", repositoryName(repo), "login", login, "error", err)
		return false
	}
	return permission.Push
}
