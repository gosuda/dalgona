package daltoki

import (
	"cmp"
	"context"
	json "encoding/json/v2"
	"errors"
	"maps"
	"net/http"
	"net/http/httptest"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/journal"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dalgona/github"
)

var testNow = time.Date(2026, 11, 24, 12, 0, 0, 0, time.UTC)

func testClock() time.Time { return testNow }

type fakeGraphs struct {
	mu     sync.Mutex
	gate   chan struct{}
	settle time.Duration
	output string
	usage  Usage
	active int
	peak   int
	runs   int
	cancel int
}

func (f *fakeGraphs) Attach(*agent.Host) {}

func (f *fakeGraphs) CancelAll(context.Context) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.cancel++
	return nil
}

func (f *fakeGraphs) cancelled() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.cancel
}

func (f *fakeGraphs) counts() (active, peak, runs int) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.active, f.peak, f.runs
}

func (f *fakeGraphs) open() {
	f.mu.Lock()
	defer f.mu.Unlock()
	close(f.gate)
}

func (f *fakeGraphs) Run(ctx context.Context, run GraphRun) (GraphResult, error) {
	f.mu.Lock()
	f.active++
	f.runs++
	f.peak = max(f.peak, f.active)
	gate, settle := f.gate, f.settle
	result := GraphResult{Final: f.output, Usage: run.Spent.plus(f.usage)}
	f.mu.Unlock()
	defer func() {
		f.mu.Lock()
		f.active--
		f.mu.Unlock()
	}()
	if gate == nil {
		return result, nil
	}
	select {
	case <-gate:
		return result, nil
	case <-ctx.Done():
		time.Sleep(settle)
		return GraphResult{Usage: result.Usage}, ctx.Err()
	}
}

type fakeWorktrees struct {
	mu      sync.Mutex
	dir     string
	closed  int
	changes map[string]string
	pushes  []PushIntent
}

func (f *fakeWorktrees) Prepare(_ context.Context, request WorktreeRequest) (Worktree, error) {
	return Worktree{Dir: f.dir, Base: request.Base, Branch: request.Branch}, nil
}

func (f *fakeWorktrees) Changes(context.Context, string) (map[string]string, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return maps.Clone(f.changes), nil
}

func (f *fakeWorktrees) Commit(context.Context, string, string) (string, error) {
	return strings.Repeat("c", 40), nil
}

func (f *fakeWorktrees) Push(_ context.Context, intent PushIntent) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.pushes = append(f.pushes, intent)
	return nil
}

func (f *fakeWorktrees) pushed() []PushIntent {
	f.mu.Lock()
	defer f.mu.Unlock()
	return slices.Clone(f.pushes)
}

func (f *fakeWorktrees) Remove(context.Context, github.Repository, string) error {
	return nil
}

func (f *fakeWorktrees) Close() error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.closed++
	return nil
}

func (f *fakeWorktrees) closedCount() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.closed
}

func fakeDeps(t *testing.T, home string) Dependencies {
	t.Helper()
	_, client := newFakeGitHub(t)
	return Dependencies{
		Home: home, Client: client, Auth: testStore(t), Clock: testClock,
		Graphs: &fakeGraphs{output: "The nil map in parse.go causes the crash."}, Worktrees: &fakeWorktrees{dir: t.TempDir()},
	}
}

type fakeGitHub struct {
	mu           sync.Mutex
	events       map[int64][]map[string]any
	missing      map[int64]bool
	permissions  map[string]string
	labelled     []map[string]any
	comments     map[int64][]map[string]any
	commentPosts int
	pulls        []map[string]any
	nextID       int64
}

func newFakeGitHub(t *testing.T) (*fakeGitHub, *github.Client) {
	t.Helper()
	fake := &fakeGitHub{
		events: map[int64][]map[string]any{}, missing: map[int64]bool{}, permissions: map[string]string{},
		comments: map[int64][]map[string]any{}, nextID: 5000,
	}
	server := httptest.NewServer(fake)
	t.Cleanup(server.Close)
	client, err := github.NewClient(github.ClientConfig{
		BaseURL: server.URL, Repositories: []github.Repository{{Owner: "octo", Name: "repo"}}, Policy: "bot",
	}, stubCredential{}, nil)
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	return fake, client
}

func (f *fakeGitHub) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	f.mu.Lock()
	status, body := f.route(r)
	f.mu.Unlock()
	raw, err := json.Marshal(body)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_, _ = w.Write(raw) //nolint:gosec // test fake serves canned JSON fixtures, not user input
}

func (f *fakeGitHub) route(r *http.Request) (int, any) {
	rest := strings.TrimPrefix(r.URL.Path, "/repos/octo/repo")
	if rest == "" {
		return http.StatusOK, map[string]any{"id": 1, "full_name": "octo/repo", "default_branch": "main"}
	}
	parts := strings.Split(strings.Trim(rest, "/"), "/")
	switch parts[0] {
	case "issues":
		return f.routeIssues(r, rest, parts)
	case "collaborators":
		return f.routeCollaborator(parts)
	case "pulls":
		if r.Method == http.MethodPost {
			return f.createPull(r)
		}
	}
	return http.StatusOK, []any{}
}

func (f *fakeGitHub) routeIssues(r *http.Request, rest string, parts []string) (int, any) {
	if rest == "/issues" && r.URL.Query().Get("labels") == daltokiLabel {
		return http.StatusOK, f.labelled
	}
	if len(parts) != 3 {
		return http.StatusOK, []any{}
	}
	number, _ := strconv.ParseInt(parts[1], 10, 64)
	switch parts[2] {
	case "events":
		if f.missing[number] {
			return http.StatusNotFound, map[string]any{"message": "Not Found"}
		}
		return http.StatusOK, f.events[number]
	case "comments":
		return f.issueComments(r, number)
	}
	return http.StatusOK, []any{}
}

func (f *fakeGitHub) routeCollaborator(parts []string) (int, any) {
	if len(parts) != 3 {
		return http.StatusOK, []any{}
	}
	return http.StatusOK, map[string]any{"permission": cmp.Or(f.permissions[parts[1]], "read"), "user": map[string]any{"login": parts[1]}}
}

func (f *fakeGitHub) createPull(r *http.Request) (int, any) {
	var payload map[string]any
	if err := json.UnmarshalRead(r.Body, &payload); err != nil {
		return http.StatusBadRequest, map[string]any{"message": err.Error()}
	}
	f.nextID++
	f.pulls = append(f.pulls, payload)
	return http.StatusCreated, map[string]any{
		"id": f.nextID, "number": 12, "html_url": "https://github.test/pull/12", "head": map[string]any{"ref": payload["head"]},
	}
}

func (f *fakeGitHub) createdPulls() []map[string]any {
	f.mu.Lock()
	defer f.mu.Unlock()
	return slices.Clone(f.pulls)
}

func (f *fakeGitHub) issueComments(r *http.Request, number int64) (int, any) {
	if r.Method != http.MethodPost {
		return http.StatusOK, f.comments[number]
	}
	var payload struct {
		Body string `json:"body"`
	}
	if err := json.UnmarshalRead(r.Body, &payload); err != nil {
		return http.StatusBadRequest, map[string]any{"message": err.Error()}
	}
	f.commentPosts++
	f.nextID++
	comment := map[string]any{"id": f.nextID, "body": payload.Body, "html_url": "https://github.test/c/" + strconv.FormatInt(f.nextID, 10)}
	f.comments[number] = append(f.comments[number], comment)
	return http.StatusCreated, comment
}

func (f *fakeGitHub) posts() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.commentPosts
}

func labelEvent(action, login string) map[string]any {
	return map[string]any{"id": 1, "event": action, "actor": map[string]any{"login": login}, "label": map[string]any{"name": daltokiLabel}}
}

func issueObservation(number int64) Observation {
	created := testNow.Add(time.Minute)
	return Observation{
		Kind: "issue", Repository: github.Repository{Owner: "octo", Name: "repo"}, ID: strconv.FormatInt(1000+number, 10),
		IssueNumber: number, Actor: "alice", ActorType: "User", State: "open",
		Title: "Crash on start", Body: "The app crashes.", CreatedAt: created, UpdatedAt: created,
	}
}

func openedEnvelope(t *testing.T, number int64) Envelope {
	t.Helper()
	body, err := json.Marshal(map[string]any{
		"action": "opened",
		"issue": map[string]any{
			"number": number, "title": "Crash on start", "body": "The app crashes.",
			"user": map[string]any{"login": "alice", "type": "User"}, "created_at": testNow.Add(time.Minute),
		},
		"sender":     map[string]any{"login": "alice"},
		"repository": map[string]any{"full_name": "Octo/Repo"},
	})
	if err != nil {
		t.Fatalf("encode webhook: %v", err)
	}
	return Envelope{DeliveryID: "delivery-" + strconv.FormatInt(number, 10), Event: "issues", ReceivedAt: testNow, BodySHA256: BodyDigest(body), Body: body}
}

func newService(t *testing.T, cfg Config, deps Dependencies) *Service {
	t.Helper()
	svc, err := New(context.Background(), cfg, deps)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	return svc
}

func waitFor(t *testing.T, what string, condition func() bool) {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for !condition() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", what)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func startService(t *testing.T, svc *Service) func() {
	t.Helper()
	runErr := make(chan error, 1)
	go func() { runErr <- svc.Run(context.Background()) }()
	waitFor(t, "service start", func() bool { return svc.Status().Running })
	return func() {
		t.Helper()
		if err := svc.Close(context.Background()); err != nil {
			t.Fatalf("Close: %v", err)
		}
		if err := <-runErr; err != nil {
			t.Fatalf("Run: %v", err)
		}
	}
}

func jobsIn(svc *Service, state JobState) int {
	count := 0
	for _, job := range svc.Jobs() {
		if job.State == state {
			count++
		}
	}
	return count
}

func admitIssues(t *testing.T, svc *Service, numbers ...int64) {
	t.Helper()
	for _, number := range numbers {
		if _, err := svc.admitObservation(context.Background(), issueObservation(number)); err != nil {
			t.Fatalf("admit issue %d: %v", number, err)
		}
	}
}

func TestAdmittedEventYieldsOneJob(t *testing.T) {
	ctx := context.Background()
	_, client := newFakeGitHub(t)
	home := t.TempDir()
	deps := fakeDeps(t, home)
	deps.Client = client
	svc := newService(t, enabledConfig(), deps)
	obs := issueObservation(7)
	updated := obs
	updated.UpdatedAt = obs.UpdatedAt.Add(time.Hour)
	requireAdmissionSequence(t, ctx, svc, obs, updated)
	requireSingleQueuedTriageJob(t, svc)
	requireJobsSurviveRestart(t, ctx, home, deps, svc)
}

func requireAdmissionSequence(t *testing.T, ctx context.Context, svc *Service, obs, updated Observation) {
	t.Helper()
	steps := []struct {
		name  string
		admit func() (bool, error)
		want  bool
	}{
		{"first sight", func() (bool, error) { return svc.admitObservation(ctx, obs) }, true},
		{"same delivery", func() (bool, error) { return svc.admitObservation(ctx, obs) }, false},
		{"later update", func() (bool, error) { return svc.admitObservation(ctx, updated) }, true},
		{"relay copy", func() (bool, error) { return svc.admitEnvelope(ctx, openedEnvelope(t, 7)) }, true},
	}
	for _, step := range steps {
		admitted, err := step.admit()
		if err != nil || admitted != step.want {
			t.Fatalf("%s: admitted=%v err=%v, want %v", step.name, admitted, err, step.want)
		}
	}
}

func requireSingleQueuedTriageJob(t *testing.T, svc *Service) {
	t.Helper()
	jobs := svc.Jobs()
	if len(jobs) != 1 || jobs[0].Kind != JobTriage || jobs[0].Issue != 7 || jobs[0].State != JobQueued {
		t.Fatalf("jobs = %+v", jobs)
	}
}

func requireJobsSurviveRestart(t *testing.T, ctx context.Context, home string, deps Dependencies, svc *Service) {
	t.Helper()
	if err := svc.Close(ctx); err != nil {
		t.Fatalf("Close: %v", err)
	}
	reopened := newService(t, enabledConfig(), deps)
	if got := len(reopened.Jobs()); got != 1 {
		t.Fatalf("jobs after restart = %d", got)
	}
	if err := reopened.Close(ctx); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if got := len(recordsOfKind(t, home, kindJobCreated)); got != 1 {
		t.Fatalf("job.created records = %d", got)
	}
}

func TestFixNeedsAttributedMaintainerLabel(t *testing.T) {
	cases := []struct {
		name    string
		events  []map[string]any
		missing bool
		want    int
	}{
		{"maintainer label", []map[string]any{labelEvent("labeled", "maria")}, false, 1},
		{"reader label", []map[string]any{labelEvent("labeled", "mallory")}, false, 0},
		{"label removed", []map[string]any{labelEvent("labeled", "maria"), labelEvent("unlabeled", "maria")}, false, 0},
		{"closed issue", []map[string]any{labelEvent("labeled", "maria"), {"id": 2, "event": "closed"}}, false, 0},
		{"history unavailable", nil, true, 0},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			gh, client := newFakeGitHub(t)
			gh.events[9], gh.missing[9] = tc.events, tc.missing
			gh.permissions["maria"], gh.permissions["mallory"] = "write", "read"
			gh.labelled = []map[string]any{{"id": 90, "number": 9, "title": "Broken parser", "body": "Fails on tabs.", "state": "open"}}
			deps := fakeDeps(t, t.TempDir())
			deps.Client = client
			svc := newService(t, enabledConfig(), deps)
			defer func() { _ = svc.Close(context.Background()) }()
			obs := Observation{
				Kind: "event", Repository: github.Repository{Owner: "octo", Name: "repo"}, ID: "77", IssueNumber: 9,
				Actor: "maria", Action: "labeled", Labels: []string{daltokiLabel}, CreatedAt: testNow, UpdatedAt: testNow,
			}
			if admitted, err := svc.admitObservation(context.Background(), obs); err != nil || !admitted {
				t.Fatalf("admitted=%v err=%v", admitted, err)
			}
			jobs := svc.Jobs()
			if len(jobs) != tc.want {
				t.Fatalf("jobs = %+v, want %d", jobs, tc.want)
			}
			if tc.want == 1 && (jobs[0].Kind != JobFix || jobs[0].Actor != "maria" || jobs[0].Title != "Broken parser") {
				t.Fatalf("fix job = %+v", jobs[0])
			}
		})
	}
}

type budgetCase struct {
	name   string
	limit  func(*Config)
	usage  Usage
	reason string
}

func TestBudgetExhaustionRefusesNewJobs(t *testing.T) {
	cases := []budgetCase{
		{"jobs per day", func(c *Config) { c.JobsPerDay = 1 }, Usage{Tokens: 10, CostUSDMicros: 10}, "daltoki: daily job budget exhausted"},
		{"daily tokens", func(c *Config) { c.JobTokenLimit, c.DailyTokenLimit = 1000, 1500 }, Usage{Tokens: 600, CostUSDMicros: 10}, "daltoki: daily token budget exhausted"},
		{"daily cost", func(c *Config) { c.JobCostLimitUSDMicros, c.DailyCostLimitUSDMicros = 1000, 1500 }, Usage{Tokens: 10, CostUSDMicros: 600}, "daltoki: daily cost budget exhausted"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			runBudgetCase(t, tc)
		})
	}
}

func runBudgetCase(t *testing.T, tc budgetCase) {
	t.Helper()
	home := t.TempDir()
	deps := fakeDeps(t, home)
	graphs := &fakeGraphs{gate: make(chan struct{}), output: "reply", usage: tc.usage}
	deps.Graphs = graphs
	cfg := enabledConfig()
	tc.limit(&cfg)
	svc := newService(t, cfg, deps)
	stop := startService(t, svc)
	admitIssues(t, svc, 7, 8)
	waitFor(t, "first job running", func() bool { active, _, _ := graphs.counts(); return active == 1 })
	graphs.open()
	waitFor(t, "budget outcome", func() bool { return jobsIn(svc, JobWaitingApproval) == 1 && jobsIn(svc, JobBlocked) == 1 })
	stop()
	requireBudgetRefusal(t, home, tc.reason)
}

func requireBudgetRefusal(t *testing.T, home, reason string) {
	t.Helper()
	if got := len(recordsOfKind(t, home, kindBudgetReserved)); got != 1 {
		t.Fatalf("budget reservations = %d", got)
	}
	var refused []string
	for _, record := range recordsOfKind(t, home, kindJobState) {
		if record.Job.State == JobBlocked {
			refused = append(refused, record.Job.Reason)
		}
	}
	if len(refused) != 1 || refused[0] != reason {
		t.Fatalf("refusals = %q, want %q", refused, reason)
	}
}

func TestBudgetWaitsForRunningReservation(t *testing.T) {
	deps := fakeDeps(t, t.TempDir())
	graphs := &fakeGraphs{gate: make(chan struct{}), output: "reply", usage: Usage{Tokens: 100, CostUSDMicros: 10}}
	deps.Graphs = graphs
	cfg := enabledConfig()
	cfg.JobTokenLimit, cfg.DailyTokenLimit = 1000, 1500
	svc := newService(t, cfg, deps)
	stop := startService(t, svc)
	defer stop()
	admitIssues(t, svc, 7, 8)
	waitFor(t, "first job running", func() bool { active, _, _ := graphs.counts(); return active == 1 })
	svc.wake()
	time.Sleep(50 * time.Millisecond)
	if blocked := jobsIn(svc, JobBlocked); blocked != 0 {
		t.Fatalf("job refused while a reservation was still held: blocked=%d", blocked)
	}
	graphs.open()
	waitFor(t, "second job after release", func() bool { return jobsIn(svc, JobWaitingApproval) == 2 })
}

func TestConcurrencyLimitHoldsUnderParallelIntake(t *testing.T) {
	deps := fakeDeps(t, t.TempDir())
	graphs := &fakeGraphs{gate: make(chan struct{}), output: "reply", usage: Usage{Tokens: 10, CostUSDMicros: 10}}
	deps.Graphs = graphs
	cfg := enabledConfig()
	cfg.Concurrency, cfg.JobsPerDay = 2, 100
	svc := newService(t, cfg, deps)
	stop := startService(t, svc)
	defer stop()
	var intake sync.WaitGroup
	for number := int64(1); number <= 8; number++ {
		intake.Go(func() {
			if _, err := svc.admitObservation(context.Background(), issueObservation(number)); err != nil {
				t.Errorf("admit issue %d: %v", number, err)
			}
		})
	}
	intake.Wait()
	waitFor(t, "two running jobs", func() bool { active, _, _ := graphs.counts(); return active == 2 })
	svc.wake()
	time.Sleep(50 * time.Millisecond)
	if running, queued := jobsIn(svc, JobRunning), jobsIn(svc, JobQueued); running != 2 || queued != 6 {
		t.Fatalf("running=%d queued=%d", running, queued)
	}
	graphs.open()
	waitFor(t, "all jobs settled", func() bool { return jobsIn(svc, JobWaitingApproval) == 8 })
	if _, peak, runs := graphs.counts(); peak != 2 || runs != 8 {
		t.Fatalf("peak=%d runs=%d", peak, runs)
	}
}

func crashMidSend(t *testing.T, home string) {
	t.Helper()
	ctx := context.Background()
	store, err := openServiceStore(ctx, serviceJournalDir(home), testClock)
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	job := &jobRecord{ID: "j-1", Kind: JobTriage, Repository: "octo/repo", Issue: 7, Title: "Crash", Trigger: "poll|1", State: JobQueued}
	limits := BudgetLimits{JobsPerDay: 5, Job: Usage{Tokens: 1000, CostUSDMicros: 1000}, Daily: Usage{Tokens: 5000, CostUSDMicros: 5000}}
	steps := []func() error{
		func() error {
			_, err := store.accept(ctx, deliveryRecord{Source: "poll", Key: "poll|1"}, job)
			return err
		},
		func() error { _, _, err := store.StartNext(ctx, limits); return err },
		func() error {
			return store.Finish(ctx, "j-1", jobOutcome{state: JobWaitingApproval, draft: &draftRecord{ID: "d-1", JobID: "j-1", Kind: DraftComment, Body: "Looks like a nil map."}})
		},
		func() error { return store.DecideDraft(ctx, "d-1", store.drafts["d-1"].Revision(), DraftApproved) },
		func() error {
			_, err := store.MarkSending(ctx, "d-1", store.drafts["d-1"].Revision())
			return err
		},
		store.Close,
	}
	for i, step := range steps {
		if err := step(); err != nil {
			t.Fatalf("step %d: %v", i, err)
		}
	}
}

func draftState(svc *Service, id string) Draft {
	for _, draft := range svc.Drafts() {
		if draft.ID == id {
			return draft
		}
	}
	return Draft{}
}

func TestRestartReconcilesInterruptedSendWithoutResend(t *testing.T) {
	t.Run("posted before crash", func(t *testing.T) {
		home := t.TempDir()
		crashMidSend(t, home)
		gh, client := newFakeGitHub(t)
		gh.comments[7] = []map[string]any{{"id": 4242, "body": "Looks like a nil map.\n\n" + draftMarker("d-1"), "html_url": "https://github.test/c/4242"}}
		deps := fakeDeps(t, home)
		deps.Client = client
		svc := newService(t, enabledConfig(), deps)
		stop := startService(t, svc)
		defer stop()
		waitFor(t, "reconciled receipt", func() bool { return draftState(svc, "d-1").State == DraftPosted })
		if draft := draftState(svc, "d-1"); draft.ReceiptID != 4242 || gh.posts() != 0 {
			t.Fatalf("draft=%+v posts=%d", draft, gh.posts())
		}
		if jobs := svc.Jobs(); jobs[0].State != JobCompleted {
			t.Fatalf("job = %+v", jobs[0])
		}
	})
	t.Run("lost before crash", func(t *testing.T) {
		home := t.TempDir()
		crashMidSend(t, home)
		gh, client := newFakeGitHub(t)
		deps := fakeDeps(t, home)
		deps.Client = client
		svc := newService(t, enabledConfig(), deps)
		stop := startService(t, svc)
		defer stop()
		waitFor(t, "uncertain draft", func() bool { return draftState(svc, "d-1").State == DraftUncertain })
		svc.wake()
		time.Sleep(50 * time.Millisecond)
		if state, posts := draftState(svc, "d-1").State, gh.posts(); state != DraftUncertain || posts != 0 {
			t.Fatalf("state=%s posts=%d", state, posts)
		}
		if err := svc.DecideDraft(context.Background(), "d-1", draftState(svc, "d-1").Revision(), true); err != nil {
			t.Fatalf("DecideDraft: %v", err)
		}
		waitFor(t, "explicit resend", func() bool { return draftState(svc, "d-1").State == DraftPosted })
		if posts := gh.posts(); posts != 1 {
			t.Fatalf("posts = %d", posts)
		}
	})
}

func TestCloseSettlesRunningJobBeforeReturning(t *testing.T) {
	home := t.TempDir()
	deps := fakeDeps(t, home)
	graphs := &fakeGraphs{gate: make(chan struct{}), settle: 100 * time.Millisecond, usage: Usage{Tokens: 70, CostUSDMicros: 5}}
	deps.Graphs = graphs
	cfg := enabledConfig()
	cfg.Concurrency = 1
	svc := newService(t, cfg, deps)
	stop := startService(t, svc)
	admitIssues(t, svc, 7, 8)
	waitFor(t, "one running job", func() bool { active, _, _ := graphs.counts(); return active == 1 })
	running := svc.Jobs()[0].ID
	stop()
	requireBarrierSettlesRunning(t, home, running)
	requireRestartRecoversSettledJobs(t, cfg, deps)
}

func requireBarrierSettlesRunning(t *testing.T, home, running string) {
	t.Helper()
	records := recordsOfKind(t, home, "")
	barrier, final, moved := scanBarrier(records, running)
	if len(moved) != 0 {
		t.Fatalf("queued job moved after the barrier: %+v", moved[0])
	}
	if barrier < 0 || final[running] != JobUncertain {
		t.Fatalf("barrier=%d running job after barrier=%q", barrier, final[running])
	}
}

func scanBarrier(records []serviceRecord, running string) (int, map[string]JobState, []*jobRecord) {
	barrier := -1
	final := map[string]JobState{}
	var moved []*jobRecord
	for i, record := range records {
		switch {
		case record.Kind == kindControlChanged:
			barrier = i
		case record.Kind == kindJobState && barrier >= 0 && record.Job.ID == running:
			final[record.Job.ID] = record.Job.State
		case record.Kind == kindJobState && barrier >= 0:
			moved = append(moved, record.Job)
		}
	}
	return barrier, final, moved
}

func requireRestartRecoversSettledJobs(t *testing.T, cfg Config, deps Dependencies) {
	t.Helper()
	restarted := newService(t, cfg, deps)
	jobs := restarted.Jobs()
	if len(jobs) != 2 || jobs[0].State != JobUncertain || jobs[0].Usage.Tokens != 70 || jobs[1].State != JobQueued {
		t.Fatalf("jobs after restart = %+v", jobs)
	}
	if err := restarted.Close(context.Background()); err != nil {
		t.Fatalf("Close: %v", err)
	}
}

func TestRestartResumesJobsAndHoldsPublicationUntilResume(t *testing.T) {
	home := t.TempDir()
	gh, client := newFakeGitHub(t)
	deps := fakeDeps(t, home)
	deps.Client = client
	graphs := &fakeGraphs{gate: make(chan struct{}), settle: 10 * time.Millisecond, output: "Check the nil map.", usage: Usage{Tokens: 70, CostUSDMicros: 5}}
	deps.Graphs = graphs
	svc := newService(t, enabledConfig(), deps)
	stop := startService(t, svc)
	admitIssues(t, svc, 7)
	waitFor(t, "running job", func() bool { active, _, _ := graphs.counts(); return active == 1 })
	stop()
	deps.Graphs = &fakeGraphs{output: "Check the nil map.", usage: Usage{Tokens: 30, CostUSDMicros: 5}}
	resumed := newService(t, enabledConfig(), deps)
	stopResumed := startService(t, resumed)
	defer stopResumed()
	waitFor(t, "requeued job settles", func() bool { return jobsIn(resumed, JobWaitingApproval) == 1 })
	job := resumed.Jobs()[0]
	if job.Usage.Tokens != 100 {
		t.Fatalf("usage across runs = %+v", job.Usage)
	}
	id := draftID(job.ID)
	if err := resumed.DecideDraft(context.Background(), id, draftState(resumed, id).Revision(), true); err != nil {
		t.Fatalf("DecideDraft: %v", err)
	}
	resumed.wake()
	time.Sleep(50 * time.Millisecond)
	if posts := gh.posts(); posts != 0 {
		t.Fatalf("published before resume: posts=%d", posts)
	}
	if err := resumed.Resume(context.Background()); err != nil {
		t.Fatalf("Resume: %v", err)
	}
	waitFor(t, "published after resume", func() bool { return draftState(resumed, id).State == DraftPosted })
	if posts := gh.posts(); posts != 1 {
		t.Fatalf("posts = %d", posts)
	}
}

func TestInvalidTransitionStopsDispatch(t *testing.T) {
	home := t.TempDir()
	store, err := openServiceStore(context.Background(), serviceJournalDir(home), testClock)
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	job := &jobRecord{ID: "j-1", Kind: JobTriage, Repository: "octo/repo", Issue: 7, Trigger: "poll|1", State: JobQueued}
	if _, acceptErr := store.accept(context.Background(), deliveryRecord{Source: "poll", Key: "poll|1"}, job); acceptErr != nil {
		t.Fatalf("accept: %v", acceptErr)
	}
	if closeErr := store.Close(); closeErr != nil {
		t.Fatalf("close store: %v", closeErr)
	}
	appendInvalidTransition(t, home)
	svc := newService(t, enabledConfig(), fakeDeps(t, home))
	defer func() { _ = svc.Close(context.Background()) }()
	requireDamagedJournalStopsDispatch(t, svc)
}

func appendInvalidTransition(t *testing.T, home string) {
	t.Helper()
	raw, err := json.Marshal(serviceRecord{
		Version: 1, Type: kindJobState, Kind: kindJobState, At: testNow,
		Job: &jobRecord{ID: "j-1", State: JobCompleted},
	})
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	j, openErr := journal.Open(serviceJournalDir(home))
	if openErr != nil {
		t.Fatalf("open journal: %v", openErr)
	}
	if _, appendErr := j.Append(context.Background(), journalSession, raw); appendErr != nil {
		t.Fatalf("append: %v", appendErr)
	}
	if closeErr := j.Close(); closeErr != nil {
		t.Fatalf("close journal: %v", closeErr)
	}
}

func requireDamagedJournalStopsDispatch(t *testing.T, svc *Service) {
	t.Helper()
	bounded, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := svc.Run(bounded); err == nil || err.Error() != "daltoki: journal is damaged" {
		t.Fatalf("Run = %v", err)
	}
	if got := svc.Status().LastError; got != "daltoki: journal is damaged" {
		t.Fatalf("LastError = %q", got)
	}
}

func fixDeps(t *testing.T, home string, changes map[string]string) (Dependencies, *fakeGitHub, *fakeWorktrees) {
	t.Helper()
	gh, client := newFakeGitHub(t)
	gh.events[9] = []map[string]any{labelEvent("labeled", "maria")}
	gh.permissions["maria"] = "write"
	gh.labelled = []map[string]any{{"id": 90, "number": 9, "title": "Broken parser", "body": "Fails on tabs.", "state": "open"}}
	trees := &fakeWorktrees{dir: t.TempDir(), changes: changes}
	deps := fakeDeps(t, home)
	deps.Client, deps.Worktrees = client, trees
	deps.Graphs = &fakeGraphs{output: "Guard the nil map in parse.go."}
	return deps, gh, trees
}

func admitFixLabel(t *testing.T, svc *Service) {
	t.Helper()
	obs := Observation{
		Kind: "event", Repository: github.Repository{Owner: "octo", Name: "repo"}, ID: "77", IssueNumber: 9,
		Actor: "maria", Action: "labeled", Labels: []string{daltokiLabel}, CreatedAt: testNow, UpdatedAt: testNow,
	}
	if _, err := svc.admitObservation(context.Background(), obs); err != nil {
		t.Fatalf("admit label: %v", err)
	}
}

func TestFixDraftPushesBeforeOpeningPullRequest(t *testing.T) {
	home := t.TempDir()
	deps, gh, trees := fixDeps(t, home, map[string]string{"parse.go": "d1"})
	svc := newService(t, enabledConfig(), deps)
	stop := startService(t, svc)
	admitFixLabel(t, svc)
	draft := requireFixDraftHeld(t, svc, trees)
	waitFor(t, "pull request", func() bool { return draftState(svc, draft.ID).State == DraftPosted })
	stop()
	requirePushBeforePullRequest(t, home, gh, trees, draft)
}

func requireFixDraftHeld(t *testing.T, svc *Service, trees *fakeWorktrees) Draft {
	t.Helper()
	waitFor(t, "fix draft", func() bool { return jobsIn(svc, JobWaitingApproval) == 1 })
	job := svc.Jobs()[0]
	draft := draftState(svc, draftID(job.ID))
	if draft.Kind != DraftPullRequest || draft.Head != "daltoki/"+job.ID || draft.Base != "main" || len(trees.pushed()) != 0 {
		t.Fatalf("draft=%+v pushes=%d", draft, len(trees.pushed()))
	}
	if err := svc.DecideDraft(context.Background(), draft.ID, draft.Revision(), true); err != nil {
		t.Fatalf("DecideDraft: %v", err)
	}
	return draft
}

func requirePushBeforePullRequest(t *testing.T, home string, gh *fakeGitHub, trees *fakeWorktrees, draft Draft) {
	t.Helper()
	pushed := PushIntent{Repository: github.Repository{Owner: "octo", Name: "repo"}, Branch: draft.Head, Commit: strings.Repeat("c", 40)}
	if pushes := trees.pushed(); len(pushes) != 1 || pushes[0] != pushed {
		t.Fatalf("pushes = %+v", pushes)
	}
	pulls := gh.createdPulls()
	if len(pulls) != 1 || pulls[0]["head"] != draft.Head || pulls[0]["base"] != "main" || pulls[0]["draft"] != true {
		t.Fatalf("pull requests = %+v", pulls)
	}
	requirePublicationOrder(t, home)
}

func requirePublicationOrder(t *testing.T, home string) {
	t.Helper()
	var order []string
	for _, record := range recordsOfKind(t, home, "") {
		if record.Kind == kindJobPush || record.Kind == kindDraftSending {
			order = append(order, record.Kind)
		}
	}
	if strings.Join(order, ",") != kindJobPush+","+kindDraftSending {
		t.Fatalf("publication order = %v", order)
	}
}

func TestFixTouchingWorkflowsFails(t *testing.T) {
	deps, gh, trees := fixDeps(t, t.TempDir(), map[string]string{"parse.go": "d1", ".github/workflows/ci.yml": "d2"})
	svc := newService(t, enabledConfig(), deps)
	stop := startService(t, svc)
	defer stop()
	admitFixLabel(t, svc)
	waitFor(t, "failed fix", func() bool { return jobsIn(svc, JobFailed) == 1 })
	if job := svc.Jobs()[0]; job.Reason != "daltoki: fix changes workflow file .github/workflows/ci.yml" {
		t.Fatalf("reason = %q", job.Reason)
	}
	if len(svc.Drafts()) != 0 || len(trees.pushed()) != 0 || len(gh.createdPulls()) != 0 {
		t.Fatalf("drafts=%d pushes=%d pulls=%d", len(svc.Drafts()), len(trees.pushed()), len(gh.createdPulls()))
	}
}

func TestStartNextHoldsJobWhenBudgetAppendFails(t *testing.T) {
	ctx := context.Background()
	home := t.TempDir()
	store, err := openServiceStore(ctx, serviceJournalDir(home), testClock)
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	job := &jobRecord{ID: "j-1", Kind: JobTriage, Repository: "octo/repo", Issue: 7, Trigger: "poll|1", State: JobQueued}
	if _, acceptErr := store.accept(ctx, deliveryRecord{Source: "poll", Key: "poll|1"}, job); acceptErr != nil {
		t.Fatalf("accept: %v", acceptErr)
	}
	if closeErr := store.Close(); closeErr != nil {
		t.Fatalf("close store: %v", closeErr)
	}
	limits := BudgetLimits{JobsPerDay: 5, Job: Usage{Tokens: 1000, CostUSDMicros: 1000}, Daily: Usage{Tokens: 5000, CostUSDMicros: 5000}}
	_, started, err := store.StartNext(ctx, limits)
	if err == nil {
		t.Fatal("StartNext succeeded with a closed journal")
	}
	if started {
		t.Fatal("job started without a durable budget reservation")
	}
	reopened, err := openServiceStore(ctx, serviceJournalDir(home), testClock)
	if err != nil {
		t.Fatalf("reopen store: %v", err)
	}
	defer func() { _ = reopened.Close() }()
	jobs := reopened.Jobs()
	if len(jobs) != 1 || jobs[0].State != JobQueued {
		t.Fatalf("jobs after failed reservation = %+v", jobs)
	}
}

func setupApprovedCommentDraft(t *testing.T, svc *Service, body string) (Draft, string) {
	t.Helper()
	ctx := context.Background()
	store := svc.store
	job := &jobRecord{ID: "j-1", Kind: JobTriage, Repository: "octo/repo", Issue: 7, Title: "Crash", Trigger: "poll|1", State: JobQueued}
	if _, err := store.accept(ctx, deliveryRecord{Source: "poll", Key: "poll|1"}, job); err != nil {
		t.Fatalf("accept: %v", err)
	}
	limits := BudgetLimits{JobsPerDay: 5, Job: Usage{Tokens: 1000, CostUSDMicros: 1000}, Daily: Usage{Tokens: 5000, CostUSDMicros: 5000}}
	if _, _, err := store.StartNext(ctx, limits); err != nil {
		t.Fatalf("StartNext: %v", err)
	}
	if err := store.Finish(ctx, "j-1", jobOutcome{state: JobWaitingApproval, draft: &draftRecord{ID: "d-1", JobID: "j-1", Kind: DraftComment, Body: body}}); err != nil {
		t.Fatalf("Finish: %v", err)
	}
	revision := store.drafts["d-1"].Revision()
	if err := store.DecideDraft(ctx, "d-1", revision, DraftApproved); err != nil {
		t.Fatalf("DecideDraft: %v", err)
	}
	draft := store.Publishable()[0]
	return draft, draft.Revision()
}

func TestPublishRejectsStaleRevision(t *testing.T) {
	gh, client := newFakeGitHub(t)
	deps := fakeDeps(t, t.TempDir())
	deps.Client = client
	svc := newService(t, enabledConfig(), deps)
	defer func() { _ = svc.Close(context.Background()) }()
	svc.publishing = true
	stale, revision := setupApprovedCommentDraft(t, svc, "first reply")
	if err := svc.store.EditDraft(context.Background(), "d-1", revision, "", "second reply"); err != nil {
		t.Fatalf("EditDraft: %v", err)
	}
	next := svc.store.drafts["d-1"].Revision()
	if err := svc.store.DecideDraft(context.Background(), "d-1", next, DraftApproved); err != nil {
		t.Fatalf("DecideDraft: %v", err)
	}
	var staleErr *StaleDraftError
	if err := svc.publish(context.Background(), stale); !errors.As(err, &staleErr) {
		t.Fatalf("publish error = %v, want stale revision", err)
	}
	if posts := gh.posts(); posts != 0 {
		t.Fatalf("stale revision published: posts=%d", posts)
	}
	if state := draftState(svc, "d-1").State; state != DraftApproved {
		t.Fatalf("draft state = %q, want approved", state)
	}
}

func TestApprovedRetryRecordsReceiptWithoutResend(t *testing.T) {
	gh, client := newFakeGitHub(t)
	gh.comments[7] = []map[string]any{{"id": 4242, "body": "Looks like a nil map.\n\n" + draftMarker("d-1"), "html_url": "https://github.test/c/4242"}}
	deps := fakeDeps(t, t.TempDir())
	deps.Client = client
	svc := newService(t, enabledConfig(), deps)
	defer func() { _ = svc.Close(context.Background()) }()
	svc.publishing = true
	_, revision := setupApprovedCommentDraft(t, svc, "Looks like a nil map.")
	retry := driveApprovedRetry(t, svc, revision)
	if !retry.Sent {
		t.Fatal("approved retry lost its earlier send")
	}
	if err := svc.publish(context.Background(), retry); err != nil {
		t.Fatalf("publish retry: %v", err)
	}
	draft := draftState(svc, "d-1")
	if draft.State != DraftPosted || draft.ReceiptID != 4242 {
		t.Fatalf("draft after retry = %+v", draft)
	}
	if jobs := svc.Jobs(); jobs[0].State != JobCompleted {
		t.Fatalf("job after retry = %+v", jobs[0])
	}
	if posts := gh.posts(); posts != 0 {
		t.Fatalf("retry resent the draft: posts=%d", posts)
	}
}

func driveApprovedRetry(t *testing.T, svc *Service, revision string) Draft {
	t.Helper()
	ctx := context.Background()
	if _, err := svc.store.MarkSending(ctx, "d-1", revision); err != nil {
		t.Fatalf("MarkSending: %v", err)
	}
	if err := svc.store.MarkUncertain(ctx, "d-1", "send timed out"); err != nil {
		t.Fatalf("MarkUncertain: %v", err)
	}
	current := svc.store.drafts["d-1"].Revision()
	if current == revision {
		t.Fatal("uncertain retry kept the sending revision")
	}
	if err := svc.store.DecideDraft(ctx, "d-1", current, DraftApproved); err != nil {
		t.Fatalf("DecideDraft retry: %v", err)
	}
	return svc.store.Publishable()[0]
}

func TestShutdownBarrierPrecedesCancelledWorkers(t *testing.T) {
	home := t.TempDir()
	deps := fakeDeps(t, home)
	deps.Graphs = &fakeGraphs{gate: make(chan struct{}), settle: 100 * time.Millisecond, usage: Usage{Tokens: 70, CostUSDMicros: 5}}
	svc := newService(t, enabledConfig(), deps)
	runCtx, cancel := context.WithCancel(context.Background())
	defer cancel()
	runErr := make(chan error, 1)
	go func() { runErr <- svc.Run(runCtx) }()
	waitFor(t, "service start", func() bool { return svc.Status().Running })
	admitIssues(t, svc, 7)
	waitFor(t, "running job", func() bool { return jobsIn(svc, JobRunning) == 1 })
	cancel()
	if err := <-runErr; !errors.Is(err, context.Canceled) {
		t.Fatalf("Run = %v, want parent cancellation", err)
	}
	waitFor(t, "uncertain job", func() bool { return jobsIn(svc, JobUncertain) == 1 })
	if err := svc.Close(context.Background()); err != nil {
		t.Fatalf("Close: %v", err)
	}
	barrier, settled := barrierOrder(t, home)
	if barrier < 0 {
		t.Fatal("shutdown recorded no barrier after parent cancellation")
	}
	if settled < 0 || barrier > settled {
		t.Fatalf("barrier=%d uncertain=%d, barrier must precede worker outcome", barrier, settled)
	}
}

func barrierOrder(t *testing.T, home string) (barrier, settled int) {
	t.Helper()
	barrier, settled = -1, -1
	for i, record := range recordsOfKind(t, home, "") {
		switch {
		case record.Kind == kindControlChanged && barrier < 0:
			barrier = i
		case record.Kind == kindJobState && record.Job != nil && record.Job.State == JobUncertain:
			settled = i
		}
	}
	return barrier, settled
}

type stubReclaimer struct {
	mu      sync.Mutex
	stopped []string
	usage   Usage
	fail    bool
}

func (s *stubReclaimer) ReclaimThread(_ context.Context, threadID string) (Usage, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.stopped = append(s.stopped, threadID)
	if s.fail {
		return Usage{}, errors.New("thread is gone")
	}
	return s.usage, nil
}

func (s *stubReclaimer) stoppedThreads() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return slices.Clone(s.stopped)
}

func startRunningJobWithThreads(t *testing.T, svc *Service, numbers ...string) Job {
	t.Helper()
	ctx := context.Background()
	job := &jobRecord{ID: "j-1", Kind: JobFix, Repository: "octo/repo", Issue: 9, Title: "Crash", Trigger: "poll|1", State: JobQueued}
	if _, err := svc.store.accept(ctx, deliveryRecord{Source: "poll", Key: "poll|1"}, job); err != nil {
		t.Fatalf("accept: %v", err)
	}
	limits := BudgetLimits{JobsPerDay: 5, Job: Usage{Tokens: 1000, CostUSDMicros: 1000}, Daily: Usage{Tokens: 5000, CostUSDMicros: 5000}}
	if _, _, err := svc.store.StartNext(ctx, limits); err != nil {
		t.Fatalf("StartNext: %v", err)
	}
	for i, node := range numbers {
		if err := svc.store.RecordThread(ctx, "j-1", node, "thread-"+strconv.Itoa(i)); err != nil {
			t.Fatalf("RecordThread: %v", err)
		}
	}
	jobs := svc.Jobs()
	if len(jobs) != 1 || jobs[0].State != JobRunning {
		t.Fatalf("jobs = %+v", jobs)
	}
	return jobs[0]
}

func TestRecoverReclaimsInterruptedThreadsBeforeRequeue(t *testing.T) {
	deps := fakeDeps(t, t.TempDir())
	svc := newService(t, enabledConfig(), deps)
	defer func() { _ = svc.Close(context.Background()) }()
	before := startRunningJobWithThreads(t, svc, "plan", "fix")
	stub := &stubReclaimer{usage: Usage{Tokens: 40, CostUSDMicros: 7}}
	svc.reclaimer = stub
	if err := svc.recoverJob(context.Background(), before); err != nil {
		t.Fatalf("recoverJob: %v", err)
	}
	if got := stub.stoppedThreads(); !slices.Equal(got, []string{"thread-0", "thread-1"}) {
		t.Fatalf("reclaimed threads = %v", got)
	}
	jobs := svc.Jobs()
	if len(jobs) != 1 || jobs[0].State != JobQueued {
		t.Fatalf("jobs after recovery = %+v", jobs)
	}
	if jobs[0].Usage.Tokens != 80 || jobs[0].Usage.CostUSDMicros != 14 {
		t.Fatalf("usage after recovery = %+v, want reclaimed total", jobs[0].Usage)
	}
}

func TestRecoverRequeuesWhenThreadsAreGone(t *testing.T) {
	deps := fakeDeps(t, t.TempDir())
	svc := newService(t, enabledConfig(), deps)
	defer func() { _ = svc.Close(context.Background()) }()
	before := startRunningJobWithThreads(t, svc, "triage")
	svc.reclaimer = &stubReclaimer{fail: true}
	if err := svc.recoverJob(context.Background(), before); err != nil {
		t.Fatalf("recoverJob: %v", err)
	}
	jobs := svc.Jobs()
	if len(jobs) != 1 || jobs[0].State != JobQueued {
		t.Fatalf("jobs after recovery = %+v", jobs)
	}
	if jobs[0].Usage.Tokens != 0 || jobs[0].Usage.CostUSDMicros != 0 {
		t.Fatalf("usage after lost threads = %+v", jobs[0].Usage)
	}
}

func TestThreadUsageSumsAssistantMessages(t *testing.T) {
	price := model.Price{InputMicros: 1000000, OutputMicros: 1000000, CacheReadMicros: 1000000, CacheWriteMicros: 1000000}
	records := []event.Seqd{
		{Seq: 1, Event: event.TurnStarted{TurnID: "t"}},
		{Seq: 2, Event: event.AssistantMessage{TurnID: "t", Usage: event.Usage{Input: 3, Output: 1}}},
		{Seq: 3, Event: event.TurnCompleted{TurnID: "t", Status: "interrupted"}},
		{Seq: 4, Event: event.AssistantMessage{TurnID: "t", Usage: event.Usage{CacheRead: 2}}},
	}
	got := threadUsage(records, price)
	if got.Tokens != 6 {
		t.Fatalf("tokens = %d, want 6", got.Tokens)
	}
	if got.CostUSDMicros <= 0 {
		t.Fatalf("cost not priced: %+v", got)
	}
}
