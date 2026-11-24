package main

import (
	"context"
	json "encoding/json/v2"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/daltoki"
	"github.com/gosuda/dalgona/internal/dalgona/github"
)

var daltokiNow = time.Date(2026, 11, 24, 12, 0, 0, 0, time.UTC)

type daltokiDraftResult struct {
	ID         string `json:"id"`
	Kind       string `json:"kind"`
	Repository string `json:"repository"`
	Issue      int64  `json:"issue"`
	Body       string `json:"body"`
	State      string `json:"state"`
	Revision   string `json:"revision"`
}

type daltokiReadResult struct {
	Enabled           bool                 `json:"enabled"`
	Running           bool                 `json:"running"`
	PublicationPaused bool                 `json:"publicationPaused"`
	OpenDrafts        int                  `json:"openDrafts"`
	Drafts            []daltokiDraftResult `json:"drafts"`
}

type daltokiGitHub struct {
	mu       sync.Mutex
	comments []map[string]any
	posts    int
}

func (g *daltokiGitHub) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	g.mu.Lock()
	defer g.mu.Unlock()
	w.Header().Set("Content-Type", "application/json")
	var body any = []any{}
	switch {
	case r.URL.Path == "/repos/octo/repo":
		body = map[string]any{"id": 1, "full_name": "octo/repo", "default_branch": "main"}
	case r.URL.Path == "/repos/octo/repo/issues" && r.URL.Query().Get("labels") == "":
		created := daltokiNow.Add(time.Minute)
		body = []map[string]any{{
			"id": 70, "number": 7, "title": "Crash on start", "body": "The app crashes.", "state": "open",
			"user": map[string]any{"login": "alice", "type": "User"}, "created_at": created, "updated_at": created,
		}}
	case r.URL.Path == "/repos/octo/repo/issues/7/comments" && r.Method == http.MethodPost:
		body = g.post(r)
		w.WriteHeader(http.StatusCreated)
	case r.URL.Path == "/repos/octo/repo/issues/7/comments":
		body = g.comments
	}
	raw, _ := json.Marshal(body)
	_, _ = w.Write(raw)
}

func (g *daltokiGitHub) post(r *http.Request) map[string]any {
	var payload struct {
		Body string `json:"body"`
	}
	_ = json.UnmarshalRead(r.Body, &payload)
	g.posts++
	comment := map[string]any{"id": 900 + g.posts, "body": payload.Body, "html_url": "https://github.test/c"}
	g.comments = append(g.comments, comment)
	return comment
}

func (g *daltokiGitHub) postCount() int {
	g.mu.Lock()
	defer g.mu.Unlock()
	return g.posts
}

func (g *daltokiGitHub) firstBody() string {
	g.mu.Lock()
	defer g.mu.Unlock()
	if len(g.comments) == 0 {
		return ""
	}
	body, _ := g.comments[0]["body"].(string)
	return body
}

type daltokiCredential struct{}

func (daltokiCredential) Token(context.Context, github.CredentialRequest) (github.Token, error) {
	return github.Token{Value: "tok"}, nil
}

type replyGraphs struct{}

func (replyGraphs) Attach(*agent.Host) {}

func (replyGraphs) Run(_ context.Context, run daltoki.GraphRun) (daltoki.GraphResult, error) {
	return daltoki.GraphResult{Final: "The nil map in parse.go causes the crash.", Usage: run.Spent}, nil
}

func (replyGraphs) CancelAll(context.Context) error { return nil }

type plainWorktrees struct{ dir string }

func (p plainWorktrees) Prepare(_ context.Context, request daltoki.WorktreeRequest) (daltoki.Worktree, error) {
	return daltoki.Worktree{Dir: p.dir, Base: request.Base, Branch: request.Branch}, nil
}

func (plainWorktrees) Changes(context.Context, string) (map[string]string, error) { return nil, nil }

func (plainWorktrees) Commit(context.Context, string, string) (string, error) { return "", nil }

func (plainWorktrees) Push(context.Context, daltoki.PushIntent) error { return nil }

func (plainWorktrees) Remove(context.Context, github.Repository, string) error { return nil }

func (plainWorktrees) Close() error { return nil }

type daltokiFixture struct {
	home   string
	github *daltokiGitHub
	deps   daltoki.Dependencies
}

func newDaltokiFixture(t *testing.T) *daltokiFixture {
	t.Helper()
	fake := &daltokiGitHub{}
	server := httptest.NewServer(fake)
	t.Cleanup(server.Close)
	repos := []github.Repository{{Owner: "octo", Name: "repo"}}
	client, err := github.NewClient(github.ClientConfig{BaseURL: server.URL, Repositories: repos, Policy: "bot"}, daltokiCredential{}, nil)
	if err != nil {
		t.Fatalf("github client: %v", err)
	}
	store, err := auth.NewWithPatterns(t.TempDir(), nil)
	if err != nil {
		t.Fatalf("auth store: %v", err)
	}
	home := t.TempDir()
	return &daltokiFixture{home: home, github: fake, deps: daltoki.Dependencies{
		Home: home, Client: client, Auth: store, Clock: func() time.Time { return daltokiNow },
		Graphs: replyGraphs{}, Worktrees: plainWorktrees{dir: t.TempDir()},
	}}
}

func (f *daltokiFixture) start(t *testing.T) *daltoki.Service {
	t.Helper()
	cfg := daltoki.Config{
		Enabled: true, AppID: 1, PrivateKeyFile: "/keys/app.pem", Repositories: []github.Repository{{Owner: "octo", Name: "repo"}},
		PollInterval: time.Hour, Concurrency: 1, JobsPerDay: 5, Model: "model", Profile: "daltoki",
		JobTokenLimit: 1000, DailyTokenLimit: 5000, JobCostLimitUSDMicros: 1000, DailyCostLimitUSDMicros: 5000,
	}
	svc, err := daltoki.New(t.Context(), cfg, f.deps)
	if err != nil {
		t.Fatalf("daltoki: %v", err)
	}
	stopped := make(chan struct{})
	go func() {
		defer close(stopped)
		_ = svc.Run(context.Background())
	}()
	t.Cleanup(func() {
		_ = svc.Close(context.Background())
		<-stopped
	})
	waitUntil(t, "daltoki start", func() bool { return svc.Status().Running })
	return svc
}

func waitUntil(t *testing.T, what string, done func() bool) {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for !done() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", what)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func newDaltokiRig(t *testing.T, svc *daltoki.Service) *nativeRig {
	t.Helper()
	return newNativeRig(t, func(string) {}, func(string) rpcserver.ServeOptions {
		extensions, operations := map[string]rpcserver.Extension{}, map[string]rpcserver.Extension{}
		registerDaltoki(svc, extensions, operations)
		return rpcserver.ServeOptions{Extensions: extensions, OperationExtensions: operations}
	})
}

func (r *nativeRig) daltoki(t *testing.T) daltokiReadResult {
	t.Helper()
	var view daltokiReadResult
	if err := r.peer.Call(t.Context(), methodDaltokiRead, map[string]any{}, &view); err != nil {
		t.Fatalf("read daltoki: %v", err)
	}
	return view
}

func (r *nativeRig) openDraft(t *testing.T) daltokiDraftResult {
	t.Helper()
	var view daltokiReadResult
	waitUntil(t, "open draft", func() bool {
		view = r.daltoki(t)
		return len(view.Drafts) == 1
	})
	return view.Drafts[0]
}

func decideParams(draft daltokiDraftResult, revision, decision string) map[string]any {
	return map[string]any{"draftId": draft.ID, "expectedRevision": revision, "decision": decision, "operationId": rpc.NewOperationID()}
}

func TestDaltokiApprovalPublishesTheReadDraft(t *testing.T) {
	fixture := newDaltokiFixture(t)
	rig := newDaltokiRig(t, fixture.start(t))
	draft := rig.openDraft(t)
	if draft.Kind != "comment" || draft.Repository != "octo/repo" || draft.Issue != 7 || draft.State != "pending" {
		t.Fatalf("draft = %+v", draft)
	}
	requireStaleDecideRefused(t, rig, draft, fixture)
	requireApprovePublishesDraft(t, rig, draft, fixture)
}

func requireStaleDecideRefused(t *testing.T, rig *nativeRig, draft daltokiDraftResult, fixture *daltokiFixture) {
	t.Helper()
	stale := rig.refused(t, methodDaltokiDecide, decideParams(draft, strings.Repeat("0", 64), "approve"), rpc.ErrorStaleRevision)
	if stale.CurrentRevision != draft.Revision || fixture.github.postCount() != 0 {
		t.Fatalf("stale refusal = %+v, posts = %d", stale, fixture.github.postCount())
	}
	rig.refused(t, methodDaltokiDecide, decideParams(draft, draft.Revision, "maybe"), rpc.ErrorInvalidParams)
}

func requireApprovePublishesDraft(t *testing.T, rig *nativeRig, draft daltokiDraftResult, fixture *daltokiFixture) {
	t.Helper()
	var decided daltokiDraftResult
	if err := rig.peer.Call(t.Context(), methodDaltokiDecide, decideParams(draft, draft.Revision, "approve"), &decided); err != nil {
		t.Fatalf("decide: %v", err)
	}
	if decided.ID != draft.ID || decided.State == "pending" {
		t.Fatalf("decided = %+v", decided)
	}
	waitUntil(t, "published draft", func() bool { return len(rig.daltoki(t).Drafts) == 0 })
	if posts := fixture.github.postCount(); posts != 1 {
		t.Fatalf("posts = %d", posts)
	}
	if body := fixture.github.firstBody(); !strings.HasPrefix(body, draft.Body) {
		t.Fatalf("posted body = %q", body)
	}
}

func TestDaltokiResumeReleasesHeldPublication(t *testing.T) {
	fixture := newDaltokiFixture(t)
	first := fixture.start(t)
	waitUntil(t, "first draft", func() bool { return len(first.Drafts()) == 1 })
	if err := first.Close(context.Background()); err != nil {
		t.Fatalf("close first service: %v", err)
	}
	rig := newDaltokiRig(t, fixture.start(t))
	if view := rig.daltoki(t); !view.Running || !view.PublicationPaused {
		t.Fatalf("view after restart = %+v", view)
	}
	draft := rig.openDraft(t)
	if err := rig.peer.Call(t.Context(), methodDaltokiDecide, decideParams(draft, draft.Revision, "approve"), nil); err != nil {
		t.Fatalf("decide: %v", err)
	}
	time.Sleep(100 * time.Millisecond)
	if posts := fixture.github.postCount(); posts != 0 {
		t.Fatalf("published before resume: posts = %d", posts)
	}
	var resumed daltokiReadResult
	if err := rig.peer.Call(t.Context(), methodDaltokiResume, map[string]any{"operationId": rpc.NewOperationID()}, &resumed); err != nil {
		t.Fatalf("resume: %v", err)
	}
	if resumed.PublicationPaused {
		t.Fatalf("resume result = %+v", resumed)
	}
	waitUntil(t, "published draft", func() bool { return fixture.github.postCount() == 1 })
	rig.refused(t, methodDaltokiResume, map[string]any{}, rpc.ErrorInvalidParams)
}

func TestDaltokiSurfaceWithoutService(t *testing.T) {
	rig := newDaltokiRig(t, nil)
	if view := rig.daltoki(t); view.Enabled || view.Running || len(view.Drafts) != 0 {
		t.Fatalf("view = %+v", view)
	}
	rig.refused(t, methodDaltokiResume, map[string]any{"operationId": rpc.NewOperationID()}, rpc.ErrorUnavailable)
}
