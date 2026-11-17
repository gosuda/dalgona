package github

import (
	"context"
	"crypto"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	json "encoding/json/v2"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"testing"
	"time"
)

type stubCredential struct {
	mu       sync.Mutex
	tokens   []string
	calls    int
	lastRepo Repository
	requests []CredentialRequest
}

func (s *stubCredential) Token(ctx context.Context, request CredentialRequest) (Token, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.calls++
	s.lastRepo = request.Repository
	s.requests = append(s.requests, request)
	if len(s.tokens) == 0 {
		return Token{}, errors.New("github: no token")
	}
	idx := s.calls - 1
	if idx >= len(s.tokens) {
		idx = len(s.tokens) - 1
	}
	return Token{Value: s.tokens[idx], ExpiresAt: time.Now().Add(time.Hour)}, nil
}

type fakeClock struct {
	now    time.Time
	sleeps []time.Duration
}

func newFakeClock() *fakeClock {
	return &fakeClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
}

func (f *fakeClock) install(client *Client) {
	client.setClockForTest(func() time.Time { return f.now }, func(ctx context.Context, d time.Duration) error {
		f.sleeps = append(f.sleeps, d)
		f.now = f.now.Add(d)
		return nil
	})
}

func testClient(t *testing.T, server *httptest.Server, policy string, cred Credential) *Client {
	t.Helper()
	client, err := NewClient(ClientConfig{
		BaseURL:      server.URL,
		Repositories: []Repository{{Owner: "octo", Name: "repo"}},
		Policy:       policy,
	}, cred, server.Client())
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	return client
}

func repo() Repository { return Repository{Owner: "octo", Name: "repo"} }

func checkAPIVersionHeaders(t *testing.T, r *http.Request) {
	t.Helper()
	if r.Header.Get("X-GitHub-Api-Version") != "2026-03-10" {
		t.Errorf("api version header = %q", r.Header.Get("X-GitHub-Api-Version"))
	}
	if r.Header.Get("Accept") != "application/vnd.github+json" {
		t.Errorf("accept header = %q", r.Header.Get("Accept"))
	}
}

func assertRepositoryReads(t *testing.T, info RepositoryInfo, meta ResponseMeta, gotAuth string) {
	t.Helper()
	if info.ID != 42 || info.FullName != "octo/repo" || info.DefaultBranch != "main" {
		t.Fatalf("info = %+v", info)
	}
	if gotAuth != "Bearer tok-1" {
		t.Fatalf("authorization = %q", gotAuth)
	}
	if meta.Limit != 5000 || meta.Remaining != 4999 || meta.ETag != `"abc"` {
		t.Fatalf("meta = %+v", meta)
	}
	if meta.Reset.IsZero() {
		t.Fatal("meta.Reset not parsed")
	}
}

func TestRepositoryReadsAndMeta(t *testing.T) {
	var gotAuth string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotAuth = r.Header.Get("Authorization")
		checkAPIVersionHeaders(t, r)
		w.Header().Set("X-RateLimit-Limit", "5000")
		w.Header().Set("X-RateLimit-Remaining", "4999")
		w.Header().Set("X-RateLimit-Reset", "1893456000")
		w.Header().Set("X-GitHub-Request-Id", "req-1")
		w.Header().Set("ETag", `"abc"`)
		fmt.Fprint(w, `{"id":42,"full_name":"octo/repo","default_branch":"main","clone_url":"https://example.test/repo.git"}`)
	}))
	defer server.Close()

	cred := &stubCredential{tokens: []string{"tok-1"}}
	client := testClient(t, server, policyBot, cred)
	info, meta, err := client.Repository(context.Background(), repo())
	if err != nil {
		t.Fatalf("Repository: %v", err)
	}
	assertRepositoryReads(t, info, meta, gotAuth)
}

func TestRepositoryNotAllowlisted(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		t.Error("unexpected request")
	}))
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	_, _, err := client.Repository(context.Background(), Repository{Owner: "evil", Name: "repo"})
	if err == nil || err.Error() != "github: repository is not allowlisted" {
		t.Fatalf("err = %v", err)
	}
}

func TestNotModifiedIsSuccess(t *testing.T) {
	calls := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		w.Header().Set("ETag", `"v1"`)
		w.WriteHeader(http.StatusNotModified)
	}))
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	page, meta, err := client.Issues(context.Background(), IssuesRequest{
		Repository: repo(),
		ETag:       `"v1"`,
	})
	if err != nil {
		t.Fatalf("Issues: %v", err)
	}
	if !meta.NotModified || meta.Status != 304 {
		t.Fatalf("meta = %+v", meta)
	}
	if len(page.Items) != 0 {
		t.Fatalf("items = %+v", page.Items)
	}
	if calls != 1 {
		t.Fatalf("calls = %d", calls)
	}
}

func newHijackFlakyServer(t *testing.T, calls *int, failures int, successBody string) *httptest.Server {
	t.Helper()
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		*calls++
		if *calls <= failures {
			hj, ok := w.(http.Hijacker)
			if !ok {
				t.Fatal("no hijacker")
			}
			conn, _, hijackErr := hj.Hijack()
			if hijackErr != nil {
				t.Fatal(hijackErr)
			}
			_ = conn.Close()
			return
		}
		fmt.Fprint(w, successBody)
	}))
}

func TestBotReadRetriesTransportFailure(t *testing.T) {
	calls := 0
	server := newHijackFlakyServer(t, &calls, 2, `{"id":7,"full_name":"octo/repo","default_branch":"main","clone_url":"x"}`)
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	clock := newFakeClock()
	clock.install(client)
	info, _, err := client.Repository(context.Background(), repo())
	if err != nil {
		t.Fatalf("Repository: %v", err)
	}
	if info.ID != 7 {
		t.Fatalf("info = %+v", info)
	}
	if calls != 3 {
		t.Fatalf("calls = %d", calls)
	}
	want := []time.Duration{time.Second, 2 * time.Second}
	if len(clock.sleeps) != len(want) {
		t.Fatalf("clock.sleeps = %v", clock.sleeps)
	}
	for i := range want {
		if clock.sleeps[i] != want[i] {
			t.Fatalf("sleeps = %v, want %v", clock.sleeps, want)
		}
	}
}

func TestBotReadRetries5xx(t *testing.T) {
	calls := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		if calls == 1 {
			w.WriteHeader(http.StatusServiceUnavailable)
			return
		}
		fmt.Fprint(w, `{"id":9,"full_name":"octo/repo","default_branch":"main","clone_url":"x"}`)
	}))
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	clock := newFakeClock()
	clock.install(client)
	info, _, err := client.Repository(context.Background(), repo())
	if err != nil {
		t.Fatalf("Repository: %v", err)
	}
	if info.ID != 9 || calls != 2 {
		t.Fatalf("info=%+v calls=%d", info, calls)
	}
	if len(clock.sleeps) != 1 || clock.sleeps[0] != time.Second {
		t.Fatalf("clock.sleeps = %v", clock.sleeps)
	}
}

func TestRateWaitHonorsRetryAfter(t *testing.T) {
	calls := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		if calls == 1 {
			w.Header().Set("Retry-After", "7")
			w.Header().Set("X-RateLimit-Remaining", "0")
			w.WriteHeader(http.StatusForbidden)
			fmt.Fprint(w, `{"message":"API rate limit exceeded"}`)
			return
		}
		fmt.Fprint(w, `{"id":1,"full_name":"octo/repo","default_branch":"main","clone_url":"x"}`)
	}))
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	clock := newFakeClock()
	clock.install(client)
	if _, _, err := client.Repository(context.Background(), repo()); err != nil {
		t.Fatalf("Repository: %v", err)
	}
	if calls != 2 {
		t.Fatalf("calls = %d", calls)
	}
	if len(clock.sleeps) != 1 || clock.sleeps[0] != 7*time.Second {
		t.Fatalf("clock.sleeps = %v", clock.sleeps)
	}
}

func TestRateWaitSecondaryLimit(t *testing.T) {
	calls := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		if calls <= 2 {
			w.WriteHeader(http.StatusForbidden)
			fmt.Fprint(w, `{"message":"You have exceeded a secondary rate limit"}`)
			return
		}
		fmt.Fprint(w, `{"id":1,"full_name":"octo/repo","default_branch":"main","clone_url":"x"}`)
	}))
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	clock := newFakeClock()
	clock.install(client)
	if _, _, err := client.Repository(context.Background(), repo()); err != nil {
		t.Fatalf("Repository: %v", err)
	}
	want := []time.Duration{60 * time.Second, 120 * time.Second}
	if len(clock.sleeps) != len(want) {
		t.Fatalf("clock.sleeps = %v", clock.sleeps)
	}
	for i := range want {
		if clock.sleeps[i] != want[i] {
			t.Fatalf("sleeps = %v, want %v", clock.sleeps, want)
		}
	}
}

func TestRateLimitBudgetExhausted(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Retry-After", "30")
		w.Header().Set("X-RateLimit-Remaining", "0")
		w.WriteHeader(http.StatusTooManyRequests)
		fmt.Fprint(w, `{"message":"rate limited"}`)
	}))
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	clock := newFakeClock()
	clock.install(client)
	_, _, err := client.Repository(context.Background(), repo())
	if err == nil || err.Error() != "github: rate limit retry budget exceeded" {
		t.Fatalf("err = %v", err)
	}
}

func TestRateWait(t *testing.T) {
	now := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	cases := []struct {
		name string
		meta ResponseMeta
		want time.Duration
	}{
		{
			name: "retry after wins",
			meta: ResponseMeta{RetryAfter: now.Add(45 * time.Second)},
			want: 45 * time.Second,
		},
		{
			name: "maximum of retry after and reset",
			meta: ResponseMeta{RetryAfter: now.Add(45 * time.Second), Reset: now.Add(100 * time.Second)},
			want: 101 * time.Second,
		},
		{
			name: "reset plus one second",
			meta: ResponseMeta{Reset: now.Add(100 * time.Second)},
			want: 101 * time.Second,
		},
		{
			name: "past reset falls back to secondary",
			meta: ResponseMeta{Reset: now.Add(-time.Hour)},
			want: 60 * time.Second,
		},
		{
			name: "no headers uses secondary",
			meta: ResponseMeta{},
			want: 60 * time.Second,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var state rateState
			if got := rateWait(tc.meta, &state, now); got != tc.want {
				t.Fatalf("rateWait = %v, want %v", got, tc.want)
			}
		})
	}
	t.Run("secondary escalates", func(t *testing.T) {
		var state rateState
		want := []time.Duration{60 * time.Second, 120 * time.Second, 240 * time.Second, 240 * time.Second}
		for i, w := range want {
			if got := rateWait(ResponseMeta{}, &state, now); got != w {
				t.Fatalf("wait %d = %v, want %v", i, got, w)
			}
		}
	})
	t.Run("not rate limited without markers", func(t *testing.T) {
		meta := ResponseMeta{Limit: 5000, Remaining: 4999, Reset: now.Add(time.Minute)}
		if isRateLimited(http.StatusForbidden, meta, []byte(`{"message":"forbidden"}`), now) {
			t.Fatal("ordinary 403 classified as rate limited")
		}
		if !isRateLimited(http.StatusTooManyRequests, meta, nil, now) {
			t.Fatal("429 not classified as rate limited")
		}
	})
}

func assertMutationTransportAmbiguity(t *testing.T) {
	t.Helper()
	server := newHijackFlakyServer(t, new(int), 1, "")
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	_, _, err := client.CreateComment(context.Background(), CreateCommentRequest{
		Repository: repo(), Number: 5, Body: "hi",
	})
	var amb *AmbiguousWriteError
	if !errors.As(err, &amb) {
		t.Fatalf("err = %v (%T)", err, err)
	}
	if amb.Operation != "create comment" {
		t.Fatalf("operation = %q", amb.Operation)
	}
}

func assertMutation5xxAmbiguity(t *testing.T) {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("X-GitHub-Request-Id", "req-9")
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	_, _, err := client.CreateComment(context.Background(), CreateCommentRequest{
		Repository: repo(), Number: 5, Body: "hi",
	})
	var amb *AmbiguousWriteError
	if !errors.As(err, &amb) {
		t.Fatalf("err = %v (%T)", err, err)
	}
	if amb.RequestID != "req-9" {
		t.Fatalf("request id = %q", amb.RequestID)
	}
}

func assertMutationTerminal422(t *testing.T) {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusUnprocessableEntity)
		fmt.Fprint(w, `{"message":"Validation Failed"}`)
	}))
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	_, _, err := client.CreateComment(context.Background(), CreateCommentRequest{
		Repository: repo(), Number: 5, Body: "hi",
	})
	var apiErr *APIError
	if !errors.As(err, &apiErr) {
		t.Fatalf("err = %v (%T)", err, err)
	}
	if apiErr.Status != 422 || apiErr.Error() != "github: create comment: status 422" {
		t.Fatalf("apiErr = %+v", apiErr)
	}
}

func TestMutationAmbiguity(t *testing.T) {
	t.Run("transport error", func(t *testing.T) { assertMutationTransportAmbiguity(t) })
	t.Run("5xx", func(t *testing.T) { assertMutation5xxAmbiguity(t) })
	t.Run("terminal 422", func(t *testing.T) { assertMutationTerminal422(t) })
}

func TestMutationNotRetried(t *testing.T) {
	calls := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	_, _, _ = client.CreateComment(context.Background(), CreateCommentRequest{
		Repository: repo(), Number: 5, Body: "hi",
	})
	if calls != 1 {
		t.Fatalf("calls = %d", calls)
	}
}

func Test401RefreshOnceForReads(t *testing.T) {
	calls := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		if calls == 1 {
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		fmt.Fprint(w, `{"id":3,"full_name":"octo/repo","default_branch":"main","clone_url":"x"}`)
	}))
	defer server.Close()
	cred := &stubCredential{tokens: []string{"old", "new"}}
	client := testClient(t, server, policyBot, cred)
	info, _, err := client.Repository(context.Background(), repo())
	if err != nil {
		t.Fatalf("Repository: %v", err)
	}
	if info.ID != 3 || calls != 2 {
		t.Fatalf("info=%+v calls=%d", info, calls)
	}
	if cred.calls != 2 {
		t.Fatalf("credential calls = %d", cred.calls)
	}
}

func Test401NoRefreshForMutations(t *testing.T) {
	calls := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		w.WriteHeader(http.StatusUnauthorized)
	}))
	defer server.Close()
	cred := &stubCredential{tokens: []string{"t1", "t2"}}
	client := testClient(t, server, policyBot, cred)
	_, _, err := client.CreateComment(context.Background(), CreateCommentRequest{
		Repository: repo(), Number: 1, Body: "x",
	})
	var apiErr *APIError
	if !errors.As(err, &apiErr) || apiErr.Status != 401 {
		t.Fatalf("err = %v", err)
	}
	if calls != 1 || cred.calls != 1 {
		t.Fatalf("calls=%d cred=%d", calls, cred.calls)
	}
}

func TestReporterPolicyHasNoRetry(t *testing.T) {
	calls := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer server.Close()
	cred := &stubCredential{tokens: []string{"r1", "r2"}}
	client := testClient(t, server, policyReporter, cred)
	client.sleeper = func(ctx context.Context, d time.Duration) error {
		t.Fatalf("reporter slept %v", d)
		return nil
	}
	_, _, err := client.SearchIssues(context.Background(), IssueSearch{Query: "crash"})
	if err == nil {
		t.Fatal("expected error")
	}
	if calls != 1 || cred.calls != 1 {
		t.Fatalf("calls=%d cred=%d", calls, cred.calls)
	}
}

func TestReporterReturnsAllResponses(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Retry-After", "30")
		w.WriteHeader(http.StatusForbidden)
		fmt.Fprint(w, `{"message":"forbidden"}`)
	}))
	defer server.Close()
	client := testClient(t, server, policyReporter, &stubCredential{tokens: []string{"tok"}})
	_, meta, err := client.SearchIssues(context.Background(), IssueSearch{Query: "x"})
	var apiErr *APIError
	if !errors.As(err, &apiErr) {
		t.Fatalf("err = %v", err)
	}
	if apiErr.Status != 403 {
		t.Fatalf("status = %d", apiErr.Status)
	}
	if meta.RetryAfter.IsZero() {
		t.Fatal("retry-after not captured")
	}
}

func TestReporter401StopsWithoutRefresh(t *testing.T) {
	calls := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		w.WriteHeader(http.StatusUnauthorized)
	}))
	defer server.Close()
	cred := &stubCredential{tokens: []string{"a", "b"}}
	client := testClient(t, server, policyReporter, cred)
	_, _, err := client.SearchIssues(context.Background(), IssueSearch{Query: "x"})
	var apiErr *APIError
	if !errors.As(err, &apiErr) || apiErr.Status != 401 {
		t.Fatalf("err = %v", err)
	}
	if calls != 1 || cred.calls != 1 {
		t.Fatalf("calls=%d cred=%d", calls, cred.calls)
	}
}

func TestReporterRedirectNotFollowed(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/search/issues" {
			w.Header().Set("Location", "/elsewhere")
			w.WriteHeader(http.StatusFound)
			return
		}
		w.WriteHeader(http.StatusOK)
	}))
	defer server.Close()
	client := testClient(t, server, policyReporter, &stubCredential{tokens: []string{"tok"}})
	_, meta, err := client.SearchIssues(context.Background(), IssueSearch{Query: "x"})
	var apiErr *APIError
	if !errors.As(err, &apiErr) || apiErr.Status != 302 {
		t.Fatalf("err = %v meta=%+v", err, meta)
	}
}

func TestBotRedirectSameOrigin(t *testing.T) {
	var finalAuth string
	mux := http.NewServeMux()
	mux.HandleFunc("/repos/octo/repo", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Location", "/repos/octo/repo/")
		w.WriteHeader(http.StatusFound)
	})
	mux.HandleFunc("/repos/octo/repo/", func(w http.ResponseWriter, r *http.Request) {
		finalAuth = r.Header.Get("Authorization")
		fmt.Fprint(w, `{"id":5,"full_name":"octo/repo","default_branch":"main","clone_url":"x"}`)
	})
	server := httptest.NewServer(mux)
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	info, _, err := client.Repository(context.Background(), repo())
	if err != nil {
		t.Fatalf("Repository: %v", err)
	}
	if info.ID != 5 {
		t.Fatalf("info = %+v", info)
	}
	if finalAuth != "Bearer tok" {
		t.Fatalf("final auth = %q", finalAuth)
	}
}

func TestMutationSpacing(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusCreated)
		fmt.Fprint(w, `{"id":1,"body":"x","html_url":"u","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","user":{"login":"bot","type":"Bot"}}`)
	}))
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	clock := newFakeClock()
	clock.install(client)
	for i := 0; i < 2; i++ {
		if _, _, err := client.CreateComment(context.Background(), CreateCommentRequest{
			Repository: repo(), Number: 1, Body: "x",
		}); err != nil {
			t.Fatalf("CreateComment: %v", err)
		}
	}
	if len(clock.sleeps) != 1 || clock.sleeps[0] != time.Second {
		t.Fatalf("clock.sleeps = %v", clock.sleeps)
	}
}

func TestEmptyRequiredInput(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		t.Error("unexpected request")
	}))
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	if _, _, err := client.CreateComment(context.Background(), CreateCommentRequest{Repository: repo(), Number: 1}); err == nil || err.Error() != "github: invalid request" {
		t.Fatalf("err = %v", err)
	}
}

func TestSearchIssuesScopesQuery(t *testing.T) {
	var gotQuery string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotQuery = r.URL.Query().Get("q")
		fmt.Fprint(w, `{"total_count":0,"incomplete_results":false,"items":[]}`)
	}))
	defer server.Close()
	client := testClient(t, server, policyReporter, &stubCredential{tokens: []string{"tok"}})
	if _, _, err := client.SearchIssues(context.Background(), IssueSearch{Query: "fingerprint:abc"}); err != nil {
		t.Fatalf("SearchIssues: %v", err)
	}
	if gotQuery != "repo:octo/repo fingerprint:abc" {
		t.Fatalf("q = %q", gotQuery)
	}
}

func TestCreateIssueAndReact(t *testing.T) {
	var paths []string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		paths = append(paths, r.Method+" "+r.URL.Path)
		if strings.HasSuffix(r.URL.Path, "/issues") {
			fmt.Fprint(w, `{"id":11,"number":3,"title":"t","body":"b","html_url":"https://x","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","user":{"login":"bot","type":"Bot"},"labels":[]}`)
			return
		}
		w.WriteHeader(http.StatusCreated)
	}))
	defer server.Close()
	client := testClient(t, server, policyReporter, &stubCredential{tokens: []string{"tok"}})
	issue, _, err := client.CreateIssue(context.Background(), CreateIssue{
		Owner: "octo", Repo: "repo", Title: "t", Body: "b",
	})
	if err != nil {
		t.Fatalf("CreateIssue: %v", err)
	}
	if issue.Number != 3 || issue.Repository != "octo/repo" {
		t.Fatalf("issue = %+v", issue)
	}
	if _, err := client.ReactIssue(context.Background(), IssueReaction{
		Owner: "octo", Repo: "repo", Number: 3, Content: "eyes",
	}); err != nil {
		t.Fatalf("ReactIssue: %v", err)
	}
	want := "POST /repos/octo/repo/issues;POST /repos/octo/repo/issues/3/reactions"
	if strings.Join(paths, ";") != want {
		t.Fatalf("paths = %v", paths)
	}
}

func botSurfaceServer(t *testing.T) *httptest.Server {
	t.Helper()
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case strings.HasSuffix(r.URL.Path, "/permission"):
			fmt.Fprint(w, `{"user":{"login":"alice","type":"User"},"permission":"write"}`)
		case strings.HasSuffix(r.URL.Path, "/comments"):
			fmt.Fprint(w, `[{"id":1,"body":"b","html_url":"u","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","user":{"login":"a","type":"User"}}]`)
		case strings.HasSuffix(r.URL.Path, "/events"):
			fmt.Fprint(w, `[{"id":301,"event":"labeled","actor":{"login":"a","type":"User"},"label":{"name":"daltoki"},"created_at":"2026-01-01T00:00:00Z"}]`)
		case strings.HasSuffix(r.URL.Path, "/pulls") && r.Method == http.MethodGet:
			fmt.Fprint(w, `[{"id":2,"number":4,"html_url":"u","title":"t","draft":true,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","user":{"login":"b","type":"Bot"}}]`)
		case strings.HasSuffix(r.URL.Path, "/pulls"):
			fmt.Fprint(w, `{"id":2,"number":4,"html_url":"u","title":"t","draft":true}`)
		default:
			fmt.Fprint(w, `[]`)
		}
	}))
}

func botSurfaceClient(t *testing.T, server *httptest.Server) (*Client, context.Context) {
	t.Helper()
	return testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}}), context.Background()
}

func TestBotPermissionSurface(t *testing.T) {
	server := botSurfaceServer(t)
	defer server.Close()
	client, ctx := botSurfaceClient(t, server)
	perm, _, err := client.Permission(ctx, repo(), "alice")
	if err != nil || !perm.Push || perm.Permission != "write" {
		t.Fatalf("perm = %+v err=%v", perm, err)
	}
}

func TestBotCommentsSurface(t *testing.T) {
	server := botSurfaceServer(t)
	defer server.Close()
	client, ctx := botSurfaceClient(t, server)
	comments, _, err := client.Comments(ctx, CommentsRequest{Repository: repo(), Number: 1})
	if err != nil || len(comments.Items) != 1 || comments.Items[0].Body != "b" {
		t.Fatalf("comments = %+v err=%v", comments, err)
	}
}

func TestBotEventsSurface(t *testing.T) {
	server := botSurfaceServer(t)
	defer server.Close()
	client, ctx := botSurfaceClient(t, server)
	events, _, err := client.Events(ctx, EventsRequest{Repository: repo(), Number: 1})
	if err != nil || len(events.Items) != 1 || events.Items[0].Action != "labeled" {
		t.Fatalf("events = %+v err=%v", events, err)
	}
}

func TestBotPullRequestsSurface(t *testing.T) {
	server := botSurfaceServer(t)
	defer server.Close()
	client, ctx := botSurfaceClient(t, server)
	prs, _, err := client.PullRequests(ctx, PullRequestsRequest{Repository: repo(), Head: "feature"})
	if err != nil || len(prs.Items) != 1 || !prs.Items[0].Draft {
		t.Fatalf("prs = %+v err=%v", prs, err)
	}
	pr, _, err := client.CreatePullRequest(ctx, CreatePullRequestRequest{
		Repository: repo(), Title: "t", Head: "feature", Base: "main", Draft: true,
	})
	if err != nil || pr.Number != 4 {
		t.Fatalf("pr = %+v err=%v", pr, err)
	}
}

func TestBotChangeLabelSurface(t *testing.T) {
	var methods []string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		methods = append(methods, r.Method)
		fmt.Fprint(w, `[]`)
	}))
	defer server.Close()
	client := testClient(t, server, policyBot, &stubCredential{tokens: []string{"tok"}})
	ctx := context.Background()
	if _, _, err := client.ChangeLabel(ctx, ChangeLabelRequest{
		Repository: repo(), Number: 1, Label: "daltoki", Add: true,
	}); err != nil {
		t.Fatalf("add ChangeLabel: %v", err)
	}
	if _, _, err := client.ChangeLabel(ctx, ChangeLabelRequest{
		Repository: repo(), Number: 1, Label: "daltoki",
	}); err != nil {
		t.Fatalf("remove ChangeLabel: %v", err)
	}
	if len(methods) != 2 || methods[0] != http.MethodPost || methods[1] != http.MethodDelete {
		t.Fatalf("methods = %v", methods)
	}
}

type tokenCaptures struct {
	mu            sync.Mutex
	installations int
	tokenRequests []map[string]any
}

func newTokenCaptureServer(t *testing.T, captures *tokenCaptures) *httptest.Server {
	t.Helper()
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		captures.mu.Lock()
		defer captures.mu.Unlock()
		switch {
		case strings.HasSuffix(r.URL.Path, "/installation"):
			captures.installations++
			fmt.Fprint(w, `{"id":777}`)
		case strings.HasSuffix(r.URL.Path, "/access_tokens"):
			var body map[string]any
			raw, _ := io.ReadAll(r.Body)
			if err := json.Unmarshal(raw, &body); err != nil {
				t.Errorf("decode: %v", err)
			}
			captures.tokenRequests = append(captures.tokenRequests, body)
			w.WriteHeader(http.StatusCreated)
			fmt.Fprint(w, `{"token":"ghs_test","expires_at":"2999-01-01T00:00:00Z"}`)
		default:
			w.WriteHeader(http.StatusNotFound)
		}
	}))
}

func assertTokenPermissions(t *testing.T, body map[string]any) {
	t.Helper()
	perms, ok := body["permissions"].(map[string]any)
	if !ok {
		t.Fatalf("permissions = %v", body["permissions"])
	}
	for _, family := range []string{"issues", "pull_requests", "metadata"} {
		if _, ok := perms[family]; !ok {
			t.Fatalf("missing permission %s in %v", family, perms)
		}
	}
	if _, ok := perms["workflows"]; ok {
		t.Fatal("workflows permission requested")
	}
}

func newScopeCredential(t *testing.T, server *httptest.Server) *InstallationCredential {
	t.Helper()
	_, pemBytes := generateTestKey(t)
	keyPath := writeKeyFile(t, pemBytes, 0o600)
	cred, err := NewInstallationCredential(context.Background(), InstallationConfig{
		AppID:          12345,
		PrivateKeyFile: keyPath,
		Repositories:   []Repository{repo()},
	}, server.Client())
	if err != nil {
		t.Fatalf("NewInstallationCredential: %v", err)
	}
	cred.setBaseURLForTest(server.URL)
	return cred
}

func assertTokenExchangeScope(t *testing.T, captures *tokenCaptures) {
	t.Helper()
	captures.mu.Lock()
	defer captures.mu.Unlock()
	if captures.installations != 1 || len(captures.tokenRequests) != 1 {
		t.Fatalf("installations=%d tokenRequests=%d", captures.installations, len(captures.tokenRequests))
	}
	body := captures.tokenRequests[0]
	repos, ok := body["repositories"].([]any)
	if !ok || len(repos) != 1 || repos[0] != "repo" {
		t.Fatalf("repositories = %v", body["repositories"])
	}
	assertTokenPermissions(t, body)
}

func TestInstallationTokenScope(t *testing.T) {
	var captures tokenCaptures
	server := newTokenCaptureServer(t, &captures)
	defer server.Close()
	cred := newScopeCredential(t, server)
	token, err := cred.Token(context.Background(), CredentialRequest{Repository: repo()})
	if err != nil {
		t.Fatalf("Token: %v", err)
	}
	if token.Value != "ghs_test" {
		t.Fatalf("token = %q", token.Value)
	}
	assertTokenExchangeScope(t, &captures)
}

func TestInstallationTokenCached(t *testing.T) {
	var captures tokenCaptures
	server := newTokenCaptureServer(t, &captures)
	defer server.Close()
	cred := newScopeCredential(t, server)
	first, err := cred.Token(context.Background(), CredentialRequest{Repository: repo()})
	if err != nil || first.Value != "ghs_test" {
		t.Fatalf("first = %+v err=%v", first, err)
	}
	cached, err := cred.Token(context.Background(), CredentialRequest{Repository: repo()})
	if err != nil || cached.Value != "ghs_test" {
		t.Fatalf("cached = %+v err=%v", cached, err)
	}
	captures.mu.Lock()
	defer captures.mu.Unlock()
	if len(captures.tokenRequests) != 1 {
		t.Fatalf("token re-fetched: %d requests", len(captures.tokenRequests))
	}
}

func TestInstallationTokenRefusals(t *testing.T) {
	var captures tokenCaptures
	server := newTokenCaptureServer(t, &captures)
	defer server.Close()
	cred := newScopeCredential(t, server)
	if _, err := cred.Token(context.Background(), CredentialRequest{
		Repository: repo(), Workflow: true,
	}); err == nil {
		t.Fatal("workflow request accepted")
	}
	if _, err := cred.Token(context.Background(), CredentialRequest{
		Repository: Repository{Owner: "other", Name: "repo"},
	}); err == nil || err.Error() != "github: repository is not allowlisted" {
		t.Fatalf("err = %v", err)
	}
}

func generateTestKey(t *testing.T) (*rsa.PrivateKey, []byte) {
	t.Helper()
	key, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatal(err)
	}
	der := x509.MarshalPKCS1PrivateKey(key)
	return key, pem.EncodeToMemory(&pem.Block{Type: "RSA PRIVATE KEY", Bytes: der})
}

func writeKeyFile(t *testing.T, pemBytes []byte, perm os.FileMode) string {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "key.pem")
	if err := os.WriteFile(path, pemBytes, perm); err != nil {
		t.Fatal(err)
	}
	if runtime.GOOS == "windows" {
		setTestKeyACL(t, path, perm)
	}
	return path
}

func testCredentialWithKey(t *testing.T, appID int64) (*InstallationCredential, *rsa.PrivateKey) {
	t.Helper()
	key, pemBytes := generateTestKey(t)
	keyPath := writeKeyFile(t, pemBytes, 0o600)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNotFound)
	}))
	t.Cleanup(server.Close)
	cred, err := NewInstallationCredential(context.Background(), InstallationConfig{
		AppID:          appID,
		PrivateKeyFile: keyPath,
		Repositories:   []Repository{repo()},
	}, server.Client())
	if err != nil {
		t.Fatalf("NewInstallationCredential: %v", err)
	}
	return cred, key
}

func splitTestJWT(t *testing.T, jwt string) (signingInput string, signature, claimsJSON []byte) {
	t.Helper()
	parts := strings.Split(jwt, ".")
	if len(parts) != 3 {
		t.Fatalf("jwt parts = %d", len(parts))
	}
	sig, err := base64.RawURLEncoding.DecodeString(parts[2])
	if err != nil {
		t.Fatal(err)
	}
	claims, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		t.Fatal(err)
	}
	return parts[0] + "." + parts[1], sig, claims
}

func assertTestClaims(t *testing.T, claimsJSON []byte, issuer string, lifetime int64) {
	t.Helper()
	var claims map[string]any
	if err := json.Unmarshal(claimsJSON, &claims); err != nil {
		t.Fatal(err)
	}
	if claims["iss"] != issuer {
		t.Fatalf("iss = %v", claims["iss"])
	}
	issued := int64(claims["iat"].(float64))
	expires := int64(claims["exp"].(float64))
	if expires-issued != lifetime {
		t.Fatalf("exp-iat = %d", expires-issued)
	}
}

func TestJWTSignature(t *testing.T) {
	cred, key := testCredentialWithKey(t, 999)
	token, err := cred.appJWT()
	if err != nil {
		t.Fatalf("appJWT: %v", err)
	}
	signingInput, sig, claimsJSON := splitTestJWT(t, token)
	digest := sha256.Sum256([]byte(signingInput))
	if verifyErr := rsa.VerifyPKCS1v15(&key.PublicKey, crypto.SHA256, digest[:], sig); verifyErr != nil {
		t.Fatalf("signature verify: %v", verifyErr)
	}
	assertTestClaims(t, claimsJSON, "999", 600)
}

type rejectedKeyCase struct {
	name     string
	path     string
	contents []byte
	perm     os.FileMode
	want     string
}

func rejectedKeyCases(t *testing.T) []rejectedKeyCase {
	t.Helper()
	dir := t.TempDir()
	_, pemBytes := generateTestKey(t)
	symlink := filepath.Join(dir, "link.pem")
	if err := os.Symlink(writeKeyFile(t, pemBytes, 0o600), symlink); err != nil {
		t.Fatal(err)
	}
	//nolint:gosec // intentionally weak key: validation must reject sub-2048-bit RSA keys.
	small, err := rsa.GenerateKey(rand.Reader, 1024)
	if err != nil {
		t.Fatal(err)
	}
	smallPEM := pem.EncodeToMemory(&pem.Block{Type: "RSA PRIVATE KEY", Bytes: x509.MarshalPKCS1PrivateKey(small)})
	smallPath := writeKeyFile(t, smallPEM, 0o600)
	return []rejectedKeyCase{
		{name: "symlink", path: symlink},
		{name: "open permissions", contents: pemBytes, perm: 0o644, want: "github: private key file must be private to the current user"},
		{name: "directory", path: dir},
		{name: "bad pem", contents: []byte("not a pem"), perm: 0o600, want: "github: invalid rsa private key"},
		{name: "small key", path: smallPath, want: "github: invalid rsa private key"},
	}
}

func assertKeyRejected(t *testing.T, tc rejectedKeyCase) {
	t.Helper()
	path := tc.path
	if path == "" {
		path = writeKeyFile(t, tc.contents, tc.perm)
	}
	_, err := NewInstallationCredential(context.Background(), InstallationConfig{
		AppID: 1, PrivateKeyFile: path, Repositories: []Repository{repo()},
	}, http.DefaultClient)
	if tc.want == "" {
		if err == nil {
			t.Fatalf("%s accepted", tc.name)
		}
		return
	}
	if err == nil || err.Error() != tc.want {
		t.Fatalf("err = %v, want %q", err, tc.want)
	}
}

func TestPrivateKeyFileRejected(t *testing.T) {
	for _, tc := range rejectedKeyCases(t) {
		t.Run(tc.name, func(t *testing.T) {
			assertKeyRejected(t, tc)
		})
	}
}

func TestTokenRefreshBeforeExpiry(t *testing.T) {
	_, pemBytes := generateTestKey(t)
	keyPath := writeKeyFile(t, pemBytes, 0o600)
	var mu sync.Mutex
	var tokenCalls int
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		if strings.HasSuffix(r.URL.Path, "/installation") {
			fmt.Fprint(w, `{"id":1}`)
			return
		}
		tokenCalls++
		w.WriteHeader(http.StatusCreated)
		fmt.Fprintf(w, `{"token":"tok-%d","expires_at":"%s"}`, tokenCalls, time.Now().Add(60*time.Second).UTC().Format(time.RFC3339))
	}))
	defer server.Close()
	cred, err := NewInstallationCredential(context.Background(), InstallationConfig{
		AppID: 1, PrivateKeyFile: keyPath, Repositories: []Repository{repo()},
	}, server.Client())
	if err != nil {
		t.Fatal(err)
	}
	cred.setBaseURLForTest(server.URL)
	first, err := cred.Token(context.Background(), CredentialRequest{Repository: repo()})
	if err != nil {
		t.Fatal(err)
	}
	second, err := cred.Token(context.Background(), CredentialRequest{Repository: repo()})
	if err != nil {
		t.Fatal(err)
	}
	if first.Value == second.Value {
		t.Fatal("expected refresh within 120s of expiry")
	}
	if tokenCalls != 2 {
		t.Fatalf("tokenCalls = %d", tokenCalls)
	}
}

func TestUserCredential(t *testing.T) {
	calls := 0
	cred := NewUserCredential(func(ctx context.Context) (string, error) {
		calls++
		return "user-token", nil
	})
	token, err := cred.Token(context.Background(), CredentialRequest{Repository: repo()})
	if err != nil || token.Value != "user-token" {
		t.Fatalf("token = %+v err=%v", token, err)
	}
	empty := NewUserCredential(func(ctx context.Context) (string, error) { return "", nil })
	if _, err := empty.Token(context.Background(), CredentialRequest{}); err == nil {
		t.Fatal("empty token accepted")
	}
}

func TestNewClientValidation(t *testing.T) {
	cred := &stubCredential{tokens: []string{"t"}}
	if _, err := NewClient(ClientConfig{Policy: "bogus", Repositories: []Repository{repo()}}, cred, nil); err == nil {
		t.Fatal("bad policy accepted")
	}
	if _, err := NewClient(ClientConfig{Policy: policyBot, Repositories: []Repository{repo()}}, nil, nil); err == nil {
		t.Fatal("nil credential accepted")
	}
	if _, err := NewClient(ClientConfig{Policy: policyBot, Repositories: []Repository{{Owner: "a"}}}, cred, nil); err == nil {
		t.Fatal("invalid repo accepted")
	}
	if _, err := NewClient(ClientConfig{BaseURL: "http://example.com", Policy: policyBot, Repositories: []Repository{repo()}}, cred, nil); err == nil {
		t.Fatal("non-loopback http accepted")
	}
}
