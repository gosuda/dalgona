package daltoki

import (
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/dalgona/github"
)

type fakePollStore struct {
	mu       sync.Mutex
	cursor   PollCursor
	has      bool
	saves    []PollCursor
	admitted []Observation
	seen     map[string]bool
}

func newFakePollStore() *fakePollStore {
	return &fakePollStore{seen: make(map[string]bool)}
}

func (f *fakePollStore) LoadCursor(ctx context.Context) (PollCursor, bool, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.cursor, f.has, nil
}

func (f *fakePollStore) SaveCursor(ctx context.Context, cursor PollCursor) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.cursor = cursor
	f.has = true
	f.saves = append(f.saves, cursor)
	return nil
}

func (f *fakePollStore) Admit(ctx context.Context, obs Observation) (bool, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	key := obs.DedupKey()
	if f.seen[key] {
		return false, nil
	}
	f.seen[key] = true
	f.admitted = append(f.admitted, obs)
	return true, nil
}

func (f *fakePollStore) saved() []PollCursor {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]PollCursor(nil), f.saves...)
}

func (f *fakePollStore) observations() []Observation {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]Observation(nil), f.admitted...)
}

type routeRequest struct {
	path    string
	query   string
	headers http.Header
}

type routeHarness struct {
	server         *httptest.Server
	mu             sync.Mutex
	requests       []routeRequest
	failComments   bool
	pageCap        int
	pagesServed    int
	etagEventsSeen []string
	xPollInterval  string
	rateReset      time.Time
	labelled304    bool
}

func (h *routeHarness) record(r *http.Request) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.requests = append(h.requests, routeRequest{path: r.URL.Path, query: r.URL.RawQuery, headers: r.Header.Clone()})
}

type issueSpec struct {
	id      int64
	number  int64
	title   string
	isPR    bool
	closed  bool
	labels  []string
	updated string
}

func issueItem(spec issueSpec) map[string]any {
	state := "open"
	if spec.closed {
		state = "closed"
	}
	item := map[string]any{
		"id": spec.id, "number": spec.number, "title": spec.title, "body": spec.title + " body",
		"state": state, "html_url": fmt.Sprintf("https://github.com/octo/repo/issues/%d", spec.number),
		"labels": []any{}, "user": map[string]any{"login": "alice", "type": "User"},
		"created_at": "2026-01-01T00:00:00Z", "updated_at": spec.updated,
	}
	if spec.isPR {
		item["pull_request"] = map[string]any{"url": "https://api.github.com/repos/octo/repo/pulls/1"}
	}
	names := make([]any, 0, len(spec.labels))
	for _, name := range spec.labels {
		names = append(names, map[string]any{"name": name})
	}
	item["labels"] = names
	return item
}

func writeJSON(t *testing.T, w http.ResponseWriter, value any) {
	t.Helper()
	raw, err := json.Marshal(value)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	w.Header().Set("Content-Type", "application/json")
	_, _ = w.Write(raw)
}

func newRouteHarness(t *testing.T) *routeHarness {
	t.Helper()
	h := &routeHarness{}
	h.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		h.record(r)
		t.Helper()
		switch {
		case r.URL.Path == "/repos/octo/repo/issues" && h.pageCap > 0:
			h.servePagedIssues(w, r, t)
		case r.URL.Path == "/repos/octo/repo/issues":
			h.serveIssues(w, r, t)
		case r.URL.Path == "/repos/octo/repo/issues/comments":
			h.serveComments(w, t)
		case strings.HasPrefix(r.URL.Path, "/repos/octo/repo/issues/") && strings.HasSuffix(r.URL.Path, "/events"):
			h.serveEvents(w, r, t)
		default:
			w.WriteHeader(http.StatusNotFound)
		}
	}))
	t.Cleanup(h.server.Close)
	return h
}

func (h *routeHarness) servePagedIssues(w http.ResponseWriter, r *http.Request, t *testing.T) {
	t.Helper()
	h.mu.Lock()
	h.pagesServed++
	page := h.pagesServed
	h.mu.Unlock()
	next := fmt.Sprintf("http://%s/repos/octo/repo/issues?page=%d&per_page=100", r.Host, page+1)
	w.Header().Set("Link", `<`+next+`>; rel="next"`)
	writeJSON(t, w, []any{})
}

func (h *routeHarness) serveIssues(w http.ResponseWriter, r *http.Request, t *testing.T) {
	t.Helper()
	if strings.Contains(r.URL.RawQuery, "labels=") {
		if h.labelled304 && r.Header.Get("If-None-Match") != "" {
			w.WriteHeader(http.StatusNotModified)
			return
		}
		writeJSON(t, w, []any{
			issueItem(issueSpec{id: 107, number: 7, title: "labelled", labels: []string{"daltoki"}, updated: "2026-01-02T00:00:00Z"}),
			issueItem(issueSpec{id: 108, number: 8, title: "closed labelled", closed: true, labels: []string{"daltoki"}, updated: "2026-01-02T00:00:00Z"}),
		})
		return
	}
	if h.xPollInterval != "" {
		w.Header().Set("X-Poll-Interval", h.xPollInterval)
	}
	writeJSON(t, w, []any{
		issueItem(issueSpec{id: 101, number: 1, title: "first", labels: []string{"daltoki"}, updated: "2026-01-02T00:00:00Z"}),
		issueItem(issueSpec{id: 102, number: 2, title: "a pr", isPR: true, updated: "2026-01-02T00:00:00Z"}),
	})
}

func (h *routeHarness) serveComments(w http.ResponseWriter, t *testing.T) {
	t.Helper()
	if h.failComments {
		w.WriteHeader(http.StatusInternalServerError)
		return
	}
	if !h.rateReset.IsZero() {
		w.Header().Set("X-RateLimit-Limit", "60")
		w.Header().Set("X-RateLimit-Remaining", "0")
		w.Header().Set("X-RateLimit-Reset", strconv.FormatInt(h.rateReset.Unix(), 10))
	}
	writeJSON(t, w, []any{
		map[string]any{
			"id": 201, "body": "hello", "html_url": "https://github.com/octo/repo/issues/1#issuecomment-201",
			"user":       map[string]any{"login": "bob", "type": "User"},
			"created_at": "2026-01-02T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
		},
	})
}

func (h *routeHarness) serveEvents(w http.ResponseWriter, r *http.Request, t *testing.T) {
	t.Helper()
	if match := r.Header.Get("If-None-Match"); match != "" {
		h.mu.Lock()
		h.etagEventsSeen = append(h.etagEventsSeen, match)
		h.mu.Unlock()
		w.WriteHeader(http.StatusNotModified)
		return
	}
	w.Header().Set("ETag", `"ev1"`)
	writeJSON(t, w, []any{
		map[string]any{
			"id": 301, "event": "labeled", "actor": map[string]any{"login": "carol", "type": "User"},
			"label":      map[string]any{"name": "daltoki"},
			"created_at": "2026-01-02T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
		},
		map[string]any{
			"id": 302, "event": "commented", "actor": map[string]any{"login": "dave", "type": "User"},
			"created_at": "2026-01-02T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
		},
	})
}

func queryHas(query, key, value string) bool {
	for _, part := range strings.Split(query, "&") {
		if part == key+"="+value {
			return true
		}
	}
	return query == "" && key == ""
}

func findRequests(h *routeHarness, path string) []routeRequest {
	h.mu.Lock()
	defer h.mu.Unlock()
	var out []routeRequest
	for _, req := range h.requests {
		if req.path == path {
			out = append(out, req)
		}
	}
	return out
}

func testPoller(t *testing.T, h *routeHarness, store PollStore) *Poller {
	t.Helper()
	return NewPoller(testPollClient(t, h.server), store, PollerConfig{
		BaseURL:      h.server.URL,
		Repositories: []github.Repository{{Owner: "octo", Name: "repo"}},
		Interval:     60 * time.Second,
	}, time.Now, nil)
}

func TestInstallationPollingRoutes(t *testing.T) {
	harness := newRouteHarness(t)
	store := newFakePollStore()
	poller := testPoller(t, harness, store)

	result, err := poller.Pass(context.Background())
	if err != nil {
		t.Fatalf("Pass: %v", err)
	}
	if !result.Complete {
		t.Fatal("expected complete scan")
	}
	assertEndpointQueries(t, harness)
	assertAdmittedKinds(t, store.observations())
	if result.Cursor.NextURL != "" {
		t.Fatalf("NextURL = %q", result.Cursor.NextURL)
	}
	if result.Cursor.Since.IsZero() {
		t.Fatal("watermark did not advance")
	}
	if len(store.saved()) == 0 {
		t.Fatal("cursor never persisted")
	}
}

func assertEndpointQueries(t *testing.T, harness *routeHarness) {
	t.Helper()
	issues := findRequests(harness, "/repos/octo/repo/issues")
	if len(issues) == 0 {
		t.Fatal("issues endpoint never fetched")
	}
	if !queryHas(issues[0].query, "state", "all") || !queryHas(issues[0].query, "per_page", "100") || !strings.Contains(issues[0].query, "since=") {
		t.Fatalf("issues query = %q", issues[0].query)
	}
	assertCommentsQuery(t, harness)
	if len(findRequests(harness, "/repos/octo/repo/issues/7/events")) == 0 {
		t.Fatal("labelled issue events never fetched")
	}
	if len(findRequests(harness, "/repos/octo/repo/issues/8/events")) != 0 {
		t.Fatal("closed labelled issue must not be scanned for events")
	}
	assertLabelledListing(t, harness)
}

func assertLabelledListing(t *testing.T, harness *routeHarness) {
	t.Helper()
	for _, req := range findRequests(harness, "/repos/octo/repo/issues") {
		if queryHas(req.query, "labels", "daltoki") {
			return
		}
	}
	t.Fatal("labelled issues listing never ran")
}

func assertCommentsQuery(t *testing.T, harness *routeHarness) {
	t.Helper()
	comments := findRequests(harness, "/repos/octo/repo/issues/comments")
	if len(comments) == 0 {
		t.Fatal("repository comments endpoint never fetched")
	}
	if strings.Contains(comments[0].query, "state=") {
		t.Fatalf("comments query must not carry state: %q", comments[0].query)
	}
	if !strings.Contains(comments[0].query, "since=") {
		t.Fatalf("comments query missing since: %q", comments[0].query)
	}
}

func assertAdmittedKinds(t *testing.T, observations []Observation) {
	t.Helper()
	if len(observations) != 4 {
		t.Fatalf("admitted = %d: %+v", len(observations), observations)
	}
	kinds := map[string]int{}
	for _, obs := range observations {
		kinds[obs.Kind]++
	}
	if kinds["issue"] != 1 || kinds["comment"] != 1 || kinds["event"] != 2 {
		t.Fatalf("kinds = %v", kinds)
	}
	for _, obs := range observations {
		assertObservationFields(t, obs)
	}
}

func assertObservationFields(t *testing.T, obs Observation) {
	t.Helper()
	switch obs.Kind {
	case "issue":
		assertIssueObservation(t, obs)
	case "event":
		assertEventObservation(t, obs)
	}
}

func assertIssueObservation(t *testing.T, obs Observation) {
	t.Helper()
	if obs.IssueNumber != 1 || obs.Actor != "alice" || len(obs.Labels) != 1 || obs.Labels[0] != "daltoki" {
		t.Fatalf("issue obs = %+v", obs)
	}
}

func assertEventObservation(t *testing.T, obs Observation) {
	t.Helper()
	if obs.Ignored {
		if obs.Action == "labeled" || obs.Action == "unlabeled" {
			t.Fatalf("label action marked ignored: %+v", obs)
		}
		return
	}
	if obs.Action != "labeled" || obs.Actor != "carol" || obs.IssueNumber != 7 {
		t.Fatalf("event obs = %+v", obs)
	}
}

func TestPartialScanKeepsWatermark(t *testing.T) {
	harness := newRouteHarness(t)
	harness.pageCap = 1000
	store := newFakePollStore()
	start := time.Date(2026, 1, 3, 0, 0, 0, 0, time.UTC)
	store.cursor = PollCursor{Since: start, ScanStartedAt: start}
	store.has = true
	poller := testPoller(t, harness, store)

	result, err := poller.Pass(context.Background())
	if err != nil {
		t.Fatalf("Pass: %v", err)
	}
	if result.Complete {
		t.Fatal("expected incomplete scan")
	}
	if harness.pagesServed != 100 {
		t.Fatalf("pages served = %d", harness.pagesServed)
	}
	if result.Cursor.NextURL == "" {
		t.Fatal("continuation was not persisted")
	}
	if !result.Cursor.Since.Equal(start) {
		t.Fatalf("watermark advanced to %v", result.Cursor.Since)
	}
	saves := store.saved()
	if len(saves) != 1 {
		t.Fatalf("saves = %d", len(saves))
	}
	if !saves[0].Since.Equal(start) || saves[0].NextURL == "" {
		t.Fatalf("saved cursor = %+v", saves[0])
	}
}

func TestPollFloorOn304(t *testing.T) {
	harness := newRouteHarness(t)
	harness.xPollInterval = "90"
	harness.rateReset = time.Now().Add(600 * time.Second)
	store := newFakePollStore()
	poller := testPoller(t, harness, store)

	first, err := poller.Pass(context.Background())
	if err != nil {
		t.Fatalf("first Pass: %v", err)
	}
	if !first.Complete {
		t.Fatal("first scan should complete")
	}
	second, err := poller.Pass(context.Background())
	if err != nil {
		t.Fatalf("second Pass: %v", err)
	}
	if !second.Complete {
		t.Fatal("second scan should complete")
	}
	harness.mu.Lock()
	seen := append([]string(nil), harness.etagEventsSeen...)
	harness.mu.Unlock()
	if len(seen) == 0 {
		t.Fatal("events endpoint never saw If-None-Match")
	}
	_ = strconv.Itoa(0)
	if second.Wait < 590*time.Second || second.Wait > 601*time.Second {
		t.Fatalf("Wait = %v, want the rate deadline floor", second.Wait)
	}
}

func TestLabelled304ReusesCachedIssueNumbers(t *testing.T) {
	harness := newRouteHarness(t)
	poller := NewPoller(testPollClient(t, harness.server), nil, PollerConfig{
		BaseURL: harness.server.URL,
	}, time.Now, nil)
	repo := github.Repository{Owner: "octo", Name: "repo"}
	firstURL := poller.labelledURL(repo)
	harness.labelled304 = true

	cached := poller.newScan(PollCursor{
		Since:    time.Now().Add(-time.Hour),
		ETags:    []EndpointETag{{URL: firstURL, ETag: `"fresh"`}},
		Labelled: map[string][]int64{"octo/repo": {5}, "octo/other": {9}},
	}, true)
	numbers, err := poller.labelledIssueNumbers(context.Background(), cached, repo)
	if err != nil {
		t.Fatalf("labelledIssueNumbers: %v", err)
	}
	if len(numbers) != 1 || numbers[0] != 5 {
		t.Fatalf("304 worklist = %v, want only this repository's cached [5]", numbers)
	}

	uncached := poller.newScan(PollCursor{
		Since: time.Now().Add(-time.Hour),
		ETags: []EndpointETag{{URL: firstURL, ETag: `"stale"`}},
	}, true)
	numbers, err = poller.labelledIssueNumbers(context.Background(), uncached, repo)
	if err != nil {
		t.Fatalf("uncached labelledIssueNumbers: %v", err)
	}
	if len(numbers) != 1 || numbers[0] != 7 {
		t.Fatalf("uncached 304 worklist = %v, want refetched [7]", numbers)
	}
	if got := uncached.cursor.Labelled["octo/repo"]; len(got) != 1 || got[0] != 7 {
		t.Fatalf("refetch did not populate the cache: %+v", uncached.cursor.Labelled)
	}
}

func TestLabelledListingCutByBudgetIsNotCached(t *testing.T) {
	harness := newRouteHarness(t)
	poller := NewPoller(testPollClient(t, harness.server), nil, PollerConfig{
		BaseURL: harness.server.URL,
	}, time.Now, nil)
	state := poller.newScan(PollCursor{Since: time.Now().Add(-time.Hour)}, true)
	state.pages = pollPageLimit

	if _, err := poller.labelledIssueNumbers(context.Background(), state, github.Repository{Owner: "octo", Name: "repo"}); err != nil {
		t.Fatalf("labelledIssueNumbers: %v", err)
	}
	if len(state.cursor.Labelled) != 0 || len(state.etags) != 0 {
		t.Fatalf("incomplete listing was cached: labelled=%v etags=%v", state.cursor.Labelled, state.etags)
	}
}
