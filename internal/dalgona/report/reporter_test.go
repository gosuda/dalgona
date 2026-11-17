package report

import (
	"context"
	json "encoding/json/v2"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/failure"
)

type stubSource struct {
	mu      sync.Mutex
	access  string
	expires time.Time
	err     error
	calls   int
}

func (s *stubSource) Credential(context.Context) (Credential, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.calls++
	if s.err != nil {
		return Credential{}, s.err
	}
	return Credential{Access: s.access, Expires: s.expires}, nil
}

type manualClock struct {
	mu  sync.Mutex
	now time.Time
}

func (c *manualClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *manualClock) Advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.now = c.now.Add(d)
}

type fakeServer struct {
	mu       sync.Mutex
	requests []string
	posts    []string
	onSearch func(req *http.Request) (int, string)
	onCreate func(req *http.Request, raw []byte) (int, string)
	onReact  func(req *http.Request) int
}

func (f *fakeServer) note(entry string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.requests = append(f.requests, entry)
}

func (f *fakeServer) hitCount(prefix string) int {
	f.mu.Lock()
	defer f.mu.Unlock()
	count := 0
	for _, entry := range f.requests {
		if strings.HasPrefix(entry, prefix) {
			count++
		}
	}
	return count
}

func (f *fakeServer) reactionCount() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	count := 0
	for _, entry := range f.requests {
		if strings.Contains(entry, "/reactions") {
			count++
		}
	}
	return count
}

func (f *fakeServer) createCount() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	count := 0
	for _, request := range f.requests {
		if request == "POST /repos/gosuda/dalgona/issues" {
			count++
		}
	}
	return count
}

func (f *fakeServer) handler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("/search/issues", func(w http.ResponseWriter, req *http.Request) {
		f.note("GET /search/issues?page=" + req.URL.Query().Get("page"))
		status, payload := f.onSearch(req)
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(status)
		_, _ = io.WriteString(w, payload)
	})
	mux.HandleFunc("/repos/gosuda/dalgona/issues", func(w http.ResponseWriter, req *http.Request) {
		raw, _ := io.ReadAll(req.Body)
		f.note("POST /repos/gosuda/dalgona/issues")
		f.mu.Lock()
		f.posts = append(f.posts, string(raw))
		f.mu.Unlock()
		status, payload := f.onCreate(req, raw)
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(status)
		_, _ = io.WriteString(w, payload)
	})
	mux.HandleFunc("/repos/gosuda/dalgona/issues/", func(w http.ResponseWriter, req *http.Request) {
		f.note("POST " + req.URL.Path)
		status := f.onReact(req)
		w.WriteHeader(status)
		_, _ = io.WriteString(w, `{}`)
	})
	return mux
}

func searchPayload(total int, incomplete bool, items ...string) string {
	return `{"total_count":` + strconv.Itoa(total) + `,"incomplete_results":` + strconv.FormatBool(incomplete) + `,"items":[` + strings.Join(items, ",") + `]}`
}

func searchItem(number int64, title, repo string, pr bool) string {
	pull := "null"
	if pr {
		pull = `{"url":"https://example.com/pr"}`
	}
	return `{"id":` + strconv.FormatInt(number, 10) +
		`,"number":` + strconv.FormatInt(number, 10) +
		`,"title":` + strconv.Quote(title) +
		`,"body":"","html_url":"","pull_request":` + pull +
		`,"labels":[],"user":{"login":"octo","type":"User"},"repository_url":"https://api.github.com/repos/` + repo +
		`","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}`
}

func createdPayload(number int64, title, body string) string {
	return `{"id":1,"number":` + strconv.FormatInt(number, 10) +
		`,"title":` + strconv.Quote(title) +
		`,"body":` + strconv.Quote(body) +
		`,"html_url":"","pull_request":null,"labels":[],"user":{"login":"octo","type":"User"},"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}`
}

func exactTitle(fp string) string {
	return "[dalgona-failure:v1:" + fp + "] dalgona 1.0.0 tool read"
}

func testRecord(fp string) failure.Record {
	return failure.Record{
		SchemaVersion: 1,
		ID:            "0123456789abcdef0123456789abcdef",
		Time:          time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC),
		Product:       "dalgona",
		Version:       "1.0.0",
		OS:            "linux",
		Arch:          "amd64",
		Class:         failure.ClassTool,
		Tool:          "read",
		Error:         "boom",
		Stack:         []failure.Frame{{Function: "main.run", File: "std/main/run", Line: 10}},
		Args:          `{"path":"x"}`,
		Output:        "out",
		Fingerprint:   fp,
		ContentFree:   false,
		Reportable:    true,
		FailingStep:   &failure.FailureStep{SchemaVersion: 1, Status: "ready"},
	}
}

func openTestReporter(t *testing.T, home string, server *httptest.Server, source *stubSource, clock *manualClock) *Reporter {
	t.Helper()
	client, err := NewReporterClient(server.URL, server.Client(), "test", source)
	if err != nil {
		t.Fatalf("NewReporterClient: %v", err)
	}
	r, err := Open(context.Background(), Options{
		Home:        home,
		Client:      client,
		Credentials: source,
		Eligibility: Eligibility{Origin: failure.OriginTest, Test: true},
		Now:         clock.Now,
		Notice:      io.Discard,
	})
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	return r
}

func waitFor(t *testing.T, what string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if cond() {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", what)
}

func emptySearch(*http.Request) (int, string) {
	return http.StatusOK, searchPayload(0, false)
}

func readState(t *testing.T, home string) reporterState {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(home, reportDirName, stateFileName))
	if err != nil {
		t.Fatalf("read state: %v", err)
	}
	var state reporterState
	if err := json.Unmarshal(raw, &state); err != nil {
		t.Fatalf("decode state: %v", err)
	}
	return state
}

func TestFingerprintFromTitle(t *testing.T) {
	fp := strings.Repeat("a", 64)
	cases := []struct {
		name  string
		title string
		want  string
		ok    bool
	}{
		{"valid", exactTitle(fp), fp, true},
		{"leading text", "x " + exactTitle(fp), "", false},
		{"uppercase", "[dalgona-failure:v1:" + strings.Repeat("A", 64) + "] dalgona 1.0.0 tool read", "", false},
		{"partial", "[dalgona-failure:v1:" + strings.Repeat("a", 63) + "] dalgona 1.0.0 tool read", "", false},
		{"empty suffix", "[dalgona-failure:v1:" + fp + "] ", "", false},
		{"no marker", "dalgona 1.0.0 tool read", "", false},
		{"too long", exactTitle(fp) + strings.Repeat("x", 240), "", false},
		{"invalid utf8", exactTitle(fp) + "\xff", "", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, ok := FingerprintFromTitle(tc.title)
			if got != tc.want || ok != tc.ok {
				t.Fatalf("FingerprintFromTitle(%q) = %q, %v; want %q, %v", tc.title, got, ok, tc.want, tc.ok)
			}
		})
	}
}

func TestReporterSearchBeforeCreate(t *testing.T) {
	fp := strings.Repeat("a", 64)
	fake := &fakeServer{}
	fake.onSearch = emptySearch
	fake.onCreate = func(_ *http.Request, _ []byte) (int, string) {
		return http.StatusCreated, createdPayload(7, exactTitle(fp), "body")
	}
	fake.onReact = func(*http.Request) int { return http.StatusOK }
	server := httptest.NewServer(fake.handler())
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r := openTestReporter(t, home, server, source, clock)
	defer func() { _ = r.Close(context.Background()) }()
	if err := r.Enqueue(context.Background(), testRecord(fp)); err != nil {
		t.Fatalf("Enqueue: %v", err)
	}
	waitFor(t, "create", func() bool { return fake.createCount() == 1 })
	if err := r.Close(context.Background()); err != nil {
		t.Fatalf("Close: %v", err)
	}
	checkFiledIssue(t, fake, home, fp, 7)
}

func TestReporterEnqueueValidation(t *testing.T) {
	fake := &fakeServer{}
	fake.onSearch = emptySearch
	fake.onCreate = func(_ *http.Request, _ []byte) (int, string) {
		return http.StatusCreated, createdPayload(1, "t", "b")
	}
	fake.onReact = func(*http.Request) int { return http.StatusOK }
	server := httptest.NewServer(fake.handler())
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r := openTestReporter(t, home, server, source, clock)
	defer func() { _ = r.Close(context.Background()) }()
	ctx := context.Background()
	dal := testRecord(strings.Repeat("b", 64))
	dal.Product = "dal"
	if err := r.Enqueue(ctx, dal); err != nil {
		t.Fatalf("Enqueue dal: %v", err)
	}
	local := testRecord(strings.Repeat("c", 64))
	local.Reportable = false
	if err := r.Enqueue(ctx, local); err != nil {
		t.Fatalf("Enqueue non-reportable: %v", err)
	}
	bad := testRecord("xyz")
	if err := r.Enqueue(ctx, bad); err != nil {
		t.Fatalf("Enqueue bad fingerprint: %v", err)
	}
	valid := testRecord(strings.Repeat("d", 64))
	if err := r.Enqueue(ctx, valid); err != nil {
		t.Fatalf("Enqueue valid: %v", err)
	}
	waitFor(t, "single create", func() bool { return fake.postCount() == 1 })
	fake.mu.Lock()
	defer fake.mu.Unlock()
	if len(fake.posts) != 1 || !strings.Contains(fake.posts[0], strings.Repeat("d", 64)) {
		t.Fatalf("posts = %v", fake.posts)
	}
}

func TestReporterTitleMarkerExact(t *testing.T) {
	fp := strings.Repeat("e", 64)
	items := []string{
		searchItem(11, "prefix "+exactTitle(fp), reportFullName, false),
		searchItem(12, "[dalgona-failure:v1:"+strings.ToUpper(fp)+"] dalgona 1.0.0 tool read", reportFullName, false),
		searchItem(13, "[dalgona-failure:v1:"+fp[:63]+"] dalgona 1.0.0 tool read", reportFullName, false),
		searchItem(14, exactTitle(fp), "other/repo", false),
		searchItem(15, exactTitle(fp), reportFullName, true),
	}
	fake := &fakeServer{}
	fake.onSearch = func(*http.Request) (int, string) {
		return http.StatusOK, searchPayload(5, false, items...)
	}
	fake.onCreate = func(_ *http.Request, _ []byte) (int, string) {
		return http.StatusCreated, createdPayload(21, exactTitle(fp), "body")
	}
	fake.onReact = func(*http.Request) int { return http.StatusOK }
	server := httptest.NewServer(fake.handler())
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r := openTestReporter(t, home, server, source, clock)
	defer func() { _ = r.Close(context.Background()) }()
	if err := r.Enqueue(context.Background(), testRecord(fp)); err != nil {
		t.Fatalf("Enqueue: %v", err)
	}
	waitFor(t, "create", func() bool { return fake.createCount() == 1 })
	if got := fake.reactionCount(); got != 0 {
		t.Fatalf("reactions = %d; want 0", got)
	}
}

func TestReporterDuplicateMatchesChooseOldest(t *testing.T) {
	fp := strings.Repeat("f", 64)
	items := []string{
		searchItem(9, exactTitle(fp), reportFullName, false),
		searchItem(3, exactTitle(fp), reportFullName, false),
		searchItem(2, exactTitle(fp), reportFullName, true),
	}
	fake := &fakeServer{}
	fake.onSearch = func(*http.Request) (int, string) {
		return http.StatusOK, searchPayload(3, false, items...)
	}
	created := false
	fake.onCreate = func(_ *http.Request, _ []byte) (int, string) {
		created = true
		return http.StatusCreated, createdPayload(99, "t", "b")
	}
	fake.onReact = func(*http.Request) int { return http.StatusOK }
	server := httptest.NewServer(fake.handler())
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r := openTestReporter(t, home, server, source, clock)
	defer func() { _ = r.Close(context.Background()) }()
	if err := r.Enqueue(context.Background(), testRecord(fp)); err != nil {
		t.Fatalf("Enqueue: %v", err)
	}
	waitFor(t, "reaction", func() bool { return fake.hitCount("POST /repos/gosuda/dalgona/issues/3/reactions") == 1 })
	if created {
		t.Fatal("create called despite exact match")
	}
	state := readState(t, home)
	if len(state.Receipts) != 1 || state.Receipts[0].State != receiptFound || state.Receipts[0].IssueNumber != 3 {
		t.Fatalf("receipts = %+v", state.Receipts)
	}
}

func TestReporterRecurrenceOneEyesReaction(t *testing.T) {
	fp := strings.Repeat("1", 64)
	fake := &fakeServer{}
	fake.onSearch = emptySearch
	fake.onCreate = func(_ *http.Request, _ []byte) (int, string) {
		return http.StatusCreated, createdPayload(31, exactTitle(fp), "body")
	}
	fake.onReact = func(*http.Request) int { return http.StatusOK }
	server := httptest.NewServer(fake.handler())
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r := openTestReporter(t, home, server, source, clock)
	defer func() { _ = r.Close(context.Background()) }()
	ctx := context.Background()
	if err := r.Enqueue(ctx, testRecord(fp)); err != nil {
		t.Fatalf("Enqueue: %v", err)
	}
	waitFor(t, "file", func() bool {
		state := readState(t, home)
		return len(state.Receipts) == 1 && state.Receipts[0].State == receiptFiled
	})
	clock.Advance(61 * time.Second)
	if err := r.Enqueue(ctx, testRecord(fp)); err != nil {
		t.Fatalf("Enqueue recurrence: %v", err)
	}
	waitFor(t, "reaction", func() bool { return fake.reactionCount() == 1 })
	if err := r.Close(ctx); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := r.Drain(ctx); err != nil {
		t.Fatalf("Drain after close: %v", err)
	}
	if got := fake.reactionCount(); got != 1 {
		t.Fatalf("reactions = %d; want exactly 1", got)
	}
}

func TestReporterAmbiguousPostNeverRepeats(t *testing.T) {
	fp := strings.Repeat("2", 64)
	fake := &fakeServer{}
	fake.onSearch = emptySearch
	fake.onCreate = func(_ *http.Request, _ []byte) (int, string) {
		return http.StatusInternalServerError, `{"message":"boom"}`
	}
	fake.onReact = func(*http.Request) int { return http.StatusOK }
	server := httptest.NewServer(fake.handler())
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r := openTestReporter(t, home, server, source, clock)
	defer func() { _ = r.Close(context.Background()) }()
	if err := r.Enqueue(context.Background(), testRecord(fp)); err != nil {
		t.Fatalf("Enqueue: %v", err)
	}
	waitFor(t, "uncertain", func() bool {
		state := readState(t, home)
		return len(state.Receipts) == 1 && state.Receipts[0].State == receiptUncertain
	})
	clock.Advance(61 * time.Second)
	if err := r.Close(context.Background()); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := r.Drain(context.Background()); err != nil {
		t.Fatalf("Drain: %v", err)
	}
	time.Sleep(100 * time.Millisecond)
	if got := fake.createCount(); got != 1 {
		t.Fatalf("creates = %d; want exactly 1", got)
	}
}

func TestReporterRestrictionsLatchAcrossAuthChange(t *testing.T) {
	fp := strings.Repeat("3", 64)
	fake := &fakeServer{}
	fake.onSearch = func(*http.Request) (int, string) {
		return http.StatusForbidden, `{"message":"rate limited"}`
	}
	fake.onCreate = func(_ *http.Request, _ []byte) (int, string) {
		return http.StatusCreated, createdPayload(1, "t", "b")
	}
	fake.onReact = func(*http.Request) int { return http.StatusOK }
	server := httptest.NewServer(fake.handler())
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r := openTestReporter(t, home, server, source, clock)
	defer func() { _ = r.Close(context.Background()) }()
	if err := r.Enqueue(context.Background(), testRecord(fp)); err != nil {
		t.Fatalf("Enqueue: %v", err)
	}
	waitFor(t, "latch", func() bool { return readState(t, home).StoppedStatus == http.StatusForbidden })
	want := "failure reporting stopped after HTTP 403; review access before enabling again"
	if err := r.Drain(context.Background()); err == nil || err.Error() != want {
		t.Fatalf("Drain = %v; want %q", err, want)
	}
	before := fake.hitCount("GET /search")
	r.mu.Lock()
	r.source = &stubSource{access: "other", expires: time.Now().Add(time.Hour)}
	r.mu.Unlock()
	if err := r.Drain(context.Background()); err == nil || err.Error() != want {
		t.Fatalf("Drain after auth change = %v; want %q", err, want)
	}
	if got := fake.hitCount("GET /search"); got != before {
		t.Fatalf("search hits grew across auth change: %d -> %d", before, got)
	}
}

func TestReporterPolicyStop(t *testing.T) {
	fp := strings.Repeat("4", 64)
	fake := &fakeServer{}
	fake.onSearch = emptySearch
	fake.onCreate = func(_ *http.Request, _ []byte) (int, string) {
		return http.StatusNotFound, `{"message":"not found"}`
	}
	fake.onReact = func(*http.Request) int { return http.StatusOK }
	server := httptest.NewServer(fake.handler())
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r := openTestReporter(t, home, server, source, clock)
	defer func() { _ = r.Close(context.Background()) }()
	if err := r.Enqueue(context.Background(), testRecord(fp)); err != nil {
		t.Fatalf("Enqueue: %v", err)
	}
	want := "failure reporting stopped after HTTP 404; check repository access and issue policy"
	waitFor(t, "policy stop", func() bool {
		err := r.Drain(context.Background())
		return err != nil && err.Error() == want
	})
}

func TestReporterSearchErrorDefers(t *testing.T) {
	fp := strings.Repeat("5", 64)
	fake := &fakeServer{}
	fake.onSearch = func(*http.Request) (int, string) {
		return http.StatusBadGateway, `{"message":"bad gateway"}`
	}
	fake.onCreate = func(_ *http.Request, _ []byte) (int, string) {
		return http.StatusCreated, createdPayload(1, "t", "b")
	}
	fake.onReact = func(*http.Request) int { return http.StatusOK }
	server := httptest.NewServer(fake.handler())
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r := openTestReporter(t, home, server, source, clock)
	defer func() { _ = r.Close(context.Background()) }()
	if err := r.Enqueue(context.Background(), testRecord(fp)); err != nil {
		t.Fatalf("Enqueue: %v", err)
	}
	waitFor(t, "search attempt", func() bool { return fake.hitCount("GET /search/issues") >= 1 })
	if err := r.Close(context.Background()); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := r.Drain(context.Background()); err != nil {
		t.Fatalf("Drain: %v", err)
	}
	if got := fake.createCount(); got != 0 {
		t.Fatalf("creates = %d; want 0", got)
	}
	state := readState(t, home)
	if len(state.Receipts) != 0 {
		t.Fatalf("receipts = %+v; want none", state.Receipts)
	}
}

func TestReporterCapsSurviveRestart(t *testing.T) {
	fps := []string{
		strings.Repeat("6", 64),
		strings.Repeat("7", 64),
		strings.Repeat("8", 64),
		strings.Repeat("9", 64),
	}
	fake := &fakeServer{}
	fake.onSearch = emptySearch
	fake.onCreate = func(_ *http.Request, raw []byte) (int, string) {
		var decoded struct {
			Title string `json:"title"`
		}
		_ = json.Unmarshal(raw, &decoded)
		return http.StatusCreated, createdPayload(int64(fake.createCount()+1), decoded.Title, "b")
	}
	fake.onReact = func(*http.Request) int { return http.StatusOK }
	server := httptest.NewServer(fake.handler())
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r := openTestReporter(t, home, server, source, clock)
	ctx := context.Background()
	enqueueFingerprints(t, r, ctx, fps)
	drainToPause(t, r, ctx, clock)
	waitFor(t, "three creates", func() bool { return fake.createCount() == 3 })
	requireDrainPaused(t, "Drain", r, ctx)
	createsBefore := fake.createCount()
	if err := r.Close(ctx); err != nil {
		t.Fatalf("Close: %v", err)
	}
	r2 := openTestReporter(t, home, server, source, clock)
	defer func() { _ = r2.Close(ctx) }()
	requireDrainPaused(t, "Drain after restart", r2, ctx)
	if got := fake.createCount(); got != createsBefore {
		t.Fatalf("creates grew after restart: %d -> %d", createsBefore, got)
	}
}

func enqueueFingerprints(t *testing.T, r *Reporter, ctx context.Context, fps []string) {
	t.Helper()
	for _, fp := range fps {
		if err := r.Enqueue(ctx, testRecord(fp)); err != nil {
			t.Fatalf("Enqueue: %v", err)
		}
	}
}

func drainToPause(t *testing.T, r *Reporter, ctx context.Context, clock *manualClock) {
	t.Helper()
	wantPaused := "failure reporting paused: local limit reached"
	for i := 0; i < 6; i++ {
		clock.Advance(61 * time.Second)
		if err := r.Drain(ctx); err != nil && err.Error() == wantPaused {
			break
		}
	}
}

func requireDrainPaused(t *testing.T, what string, r *Reporter, ctx context.Context) {
	t.Helper()
	wantPaused := "failure reporting paused: local limit reached"
	if err := r.Drain(ctx); err == nil || err.Error() != wantPaused {
		t.Fatalf("%s = %v; want %q", what, err, wantPaused)
	}
}

func TestReporterIssuePrivacyTemplate(t *testing.T) {
	fp := strings.Repeat("a", 63) + "b"
	fake := &fakeServer{}
	fake.onSearch = emptySearch
	fake.onCreate = func(_ *http.Request, _ []byte) (int, string) {
		return http.StatusCreated, createdPayload(41, exactTitle(fp), "b")
	}
	fake.onReact = func(*http.Request) int { return http.StatusOK }
	server := httptest.NewServer(fake.handler())
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r := openTestReporter(t, home, server, source, clock)
	defer func() { _ = r.Close(context.Background()) }()
	record := testRecord(fp)
	record.Error = "bad <script>alert(1)</script> @someone\x01"
	record.Tool = ""
	if err := r.Enqueue(context.Background(), record); err != nil {
		t.Fatalf("Enqueue: %v", err)
	}
	waitFor(t, "create", func() bool { return fake.postCount() == 1 })
	title, body := decodePostedIssue(t, requireSinglePost(t, fake))
	requirePrivacyTitle(t, title, fp)
	requirePrivacyBody(t, body)
	requirePrivacySections(t, body)
}

func requireSinglePost(t *testing.T, fake *fakeServer) string {
	t.Helper()
	fake.mu.Lock()
	defer fake.mu.Unlock()
	if len(fake.posts) != 1 {
		t.Fatalf("posts = %d; want 1", len(fake.posts))
	}
	return fake.posts[0]
}

func decodePostedIssue(t *testing.T, raw string) (string, string) {
	t.Helper()
	var decoded struct {
		Title string `json:"title"`
		Body  string `json:"body"`
	}
	if err := json.Unmarshal([]byte(raw), &decoded); err != nil {
		t.Fatalf("decode post: %v", err)
	}
	return decoded.Title, decoded.Body
}

func requirePrivacyTitle(t *testing.T, title, fp string) {
	t.Helper()
	if len(title) > 240 {
		t.Fatalf("title too long: %d", len(title))
	}
	if got, ok := FingerprintFromTitle(title); !ok || got != fp {
		t.Fatalf("title = %q; fingerprint round-trip failed", title)
	}
}

func requirePrivacyBody(t *testing.T, body string) {
	t.Helper()
	if strings.Contains(body, "<script>") || strings.Contains(body, "@someone") {
		t.Fatalf("body leaks raw markup/mention: %q", body)
	}
	if !strings.Contains(body, "&lt;script&gt;") || !strings.Contains(body, "@\u200b") {
		t.Fatalf("body missing escapes: %q", body)
	}
	if !strings.Contains(body, "�") {
		t.Fatalf("body missing control escape: %q", body)
	}
}

func requirePrivacySections(t *testing.T, body string) {
	t.Helper()
	for _, section := range []string{"Error:", "Stack:", "Tool arguments excerpt:", "Tool output excerpt:", "Failing-step attachment (JSON):", "Details withheld: false", "Tool: runtime"} {
		if !strings.Contains(body, section) {
			t.Fatalf("body missing %q", section)
		}
	}
	if len(body) > 32768 {
		t.Fatalf("body too long: %d", len(body))
	}
}

func TestReporterQueueFullAndMissingClient(t *testing.T) {
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r, err := Open(context.Background(), Options{
		Home:        home,
		Credentials: source,
		Eligibility: Eligibility{Origin: failure.OriginTest, Test: true},
		Now:         clock.Now,
		Notice:      io.Discard,
	})
	if err != nil {
		t.Fatalf("Open without client: %v", err)
	}
	ctx := context.Background()
	fillQueueEntries(t, r, ctx)
	overflow := testRecord(strings.Repeat("f", 64))
	if overflowErr := r.Enqueue(ctx, overflow); overflowErr == nil || overflowErr.Error() != "failure reporting queue full; report kept locally" {
		t.Fatalf("Enqueue overflow = %v", overflowErr)
	}
	if closeErr := r.Close(ctx); closeErr != nil {
		t.Fatalf("Close: %v", closeErr)
	}
	if drainErr := r.Drain(ctx); drainErr == nil || drainErr.Error() != "failure reporting client unavailable" {
		t.Fatalf("Drain without client = %v", drainErr)
	}
	state := readState(t, home)
	if state.Dropped != 1 {
		t.Fatalf("dropped = %d; want 1", state.Dropped)
	}
	entries := readQueueEntries(t, home)
	if len(entries) != maxQueueEntries {
		t.Fatalf("queued = %d; want %d", len(entries), maxQueueEntries)
	}
}

func fillQueueEntries(t *testing.T, r *Reporter, ctx context.Context) {
	t.Helper()
	for i := 0; i < maxQueueEntries; i++ {
		fp := strings.Repeat("abcdef0123456789", 4)[:62] + pad2(i)
		if err := r.Enqueue(ctx, testRecord(fp)); err != nil {
			t.Fatalf("Enqueue %d: %v", i, err)
		}
	}
}

func readQueueEntries(t *testing.T, home string) []QueueEntry {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(home, reportDirName, queueFileName))
	if err != nil {
		t.Fatalf("read queue: %v", err)
	}
	var decoded queueFile
	if err := json.Unmarshal(raw, &decoded); err != nil {
		t.Fatalf("decode queue: %v", err)
	}
	return decoded.Entries
}

func pad2(i int) string {
	if i < 10 {
		return "0" + strconv.Itoa(i)
	}
	return strconv.Itoa(i)
}

func TestReporterSecondInstanceLocked(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusTeapot)
	}))
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r1 := openTestReporter(t, home, server, source, clock)
	_, err := Open(context.Background(), Options{
		Home:        home,
		Client:      r1.client,
		Credentials: source,
		Eligibility: Eligibility{Origin: failure.OriginTest, Test: true},
		Now:         clock.Now,
		Notice:      io.Discard,
	})
	if err == nil || err.Error() != "failure reporting is already running" {
		t.Fatalf("second Open = %v", err)
	}
	if err := r1.Close(context.Background()); err != nil {
		t.Fatalf("Close: %v", err)
	}
	r2 := openTestReporter(t, home, server, source, clock)
	if err := r2.Close(context.Background()); err != nil {
		t.Fatalf("Close reopened: %v", err)
	}
}

func TestReporterStateInvalid(t *testing.T) {
	home := t.TempDir()
	dir := filepath.Join(home, reportDirName)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, queueFileName), []byte("{invalid"), 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}
	_, err := Open(context.Background(), Options{
		Home:        home,
		Eligibility: Eligibility{Origin: failure.OriginTest, Test: true},
		Notice:      io.Discard,
	})
	if err == nil || err.Error() != "failure reporting stopped: state is invalid" {
		t.Fatalf("Open = %v", err)
	}
}

func TestReporterExpiredCredential(t *testing.T) {
	fp := strings.Repeat("b", 63) + "c"
	fake := &fakeServer{}
	fake.onSearch = emptySearch
	fake.onCreate = func(_ *http.Request, _ []byte) (int, string) {
		return http.StatusCreated, createdPayload(1, "t", "b")
	}
	fake.onReact = func(*http.Request) int { return http.StatusOK }
	server := httptest.NewServer(fake.handler())
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(-time.Hour)}
	r := openTestReporter(t, home, server, source, clock)
	defer func() { _ = r.Close(context.Background()) }()
	if err := r.Enqueue(context.Background(), testRecord(fp)); err != nil {
		t.Fatalf("Enqueue: %v", err)
	}
	if err := r.Close(context.Background()); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := r.Drain(context.Background()); err != nil {
		t.Fatalf("Drain: %v", err)
	}
	time.Sleep(100 * time.Millisecond)
	if got := fake.createCount(); got != 0 {
		t.Fatalf("creates = %d; want 0", got)
	}
	raw, err := os.ReadFile(filepath.Join(home, reportDirName, queueFileName))
	if err != nil {
		t.Fatalf("read queue: %v", err)
	}
	var decoded queueFile
	if err := json.Unmarshal(raw, &decoded); err != nil {
		t.Fatalf("decode queue: %v", err)
	}
	if len(decoded.Entries) != 1 {
		t.Fatalf("queued = %d; want 1 retained", len(decoded.Entries))
	}
}

func TestReporterCooldownDefersWrite(t *testing.T) {
	fp1 := strings.Repeat("c", 64)
	fp2 := strings.Repeat("d", 63) + "e"
	fake := &fakeServer{}
	fake.onSearch = emptySearch
	fake.onCreate = func(_ *http.Request, raw []byte) (int, string) {
		var decoded struct {
			Title string `json:"title"`
		}
		_ = json.Unmarshal(raw, &decoded)
		return http.StatusCreated, createdPayload(int64(fake.createCount()+1), decoded.Title, "b")
	}
	fake.onReact = func(*http.Request) int { return http.StatusOK }
	server := httptest.NewServer(fake.handler())
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r := openTestReporter(t, home, server, source, clock)
	defer func() { _ = r.Close(context.Background()) }()
	ctx := context.Background()
	if err := r.Enqueue(ctx, testRecord(fp1)); err != nil {
		t.Fatalf("Enqueue: %v", err)
	}
	waitFor(t, "first create", func() bool { return fake.createCount() == 1 })
	if err := r.Enqueue(ctx, testRecord(fp2)); err != nil {
		t.Fatalf("Enqueue: %v", err)
	}
	if err := r.Close(ctx); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := r.Drain(ctx); err != nil {
		t.Fatalf("Drain during cooldown: %v", err)
	}
	time.Sleep(100 * time.Millisecond)
	if got := fake.createCount(); got != 1 {
		t.Fatalf("creates = %d; want 1 during cooldown", got)
	}
	clock.Advance(61 * time.Second)
	if err := r.Drain(ctx); err != nil {
		t.Fatalf("Drain after cooldown: %v", err)
	}
	waitFor(t, "second create", func() bool { return fake.createCount() == 2 })
}

func TestReporterExpiryStripsExcerpts(t *testing.T) {
	fp1 := strings.Repeat("0", 64)
	fp2 := strings.Repeat("0", 63) + "1"
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	r, err := Open(context.Background(), Options{
		Home:        home,
		Credentials: source,
		Eligibility: Eligibility{Origin: failure.OriginTest, Test: true},
		Now:         clock.Now,
		Notice:      io.Discard,
	})
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	defer func() { _ = r.Close(context.Background()) }()
	ctx := context.Background()
	record := testRecord(fp1)
	record.Error = "boom"
	record.Args = `{"path":"x"}`
	record.Output = "out"
	enqueueExpiryRecord(t, r, ctx, record)
	clock.Advance(8 * 24 * time.Hour)
	enqueueExpiryRecord(t, r, ctx, testRecord(fp2))
	entries := readQueueEntries(t, home)
	if len(entries) != 2 {
		t.Fatalf("queued = %d; want 2 retained", len(entries))
	}
	requireFirstEntryStripped(t, entries, fp1)
	requireSecondEntryKept(t, entries)
}

func enqueueExpiryRecord(t *testing.T, r *Reporter, ctx context.Context, record failure.Record) {
	t.Helper()
	if err := r.Enqueue(ctx, record); err != nil {
		t.Fatalf("Enqueue: %v", err)
	}
}

func requireFirstEntryStripped(t *testing.T, entries []QueueEntry, fp1 string) {
	t.Helper()
	stripped := entries[0]
	if stripped.Fingerprint != fp1 {
		t.Fatalf("first entry = %q; want %q", stripped.Fingerprint, fp1)
	}
	if stripped.Record.Error != "" || stripped.Record.Args != "" || stripped.Record.Output != "" {
		t.Fatalf("expired entry kept excerpts: %+v", stripped.Record)
	}
}

func requireSecondEntryKept(t *testing.T, entries []QueueEntry) {
	t.Helper()
	kept := entries[1]
	if kept.Record.Error == "" || kept.Record.Args == "" || kept.Record.Output == "" {
		t.Fatalf("fresh entry lost excerpts: %+v", kept.Record)
	}
}

func checkFiledIssue(t *testing.T, fake *fakeServer, home, fp string, number int64) {
	t.Helper()
	requireSearchThenCreate(t, fake)
	title, body := decodePostedIssue(t, requireSinglePost(t, fake))
	requireIssueContent(t, title, body, fp)
	requireFiledReceipt(t, home, number)
}

func requireSearchThenCreate(t *testing.T, fake *fakeServer) {
	t.Helper()
	fake.mu.Lock()
	defer fake.mu.Unlock()
	if len(fake.requests) < 2 || fake.requests[0] != "GET /search/issues?page=1" {
		t.Fatalf("requests = %v; want search first", fake.requests)
	}
	if fake.requests[1] != "POST /repos/gosuda/dalgona/issues" {
		t.Fatalf("requests = %v; want create second", fake.requests)
	}
	if len(fake.posts) != 1 {
		t.Fatalf("posts = %d; want 1", len(fake.posts))
	}
}

func requireIssueContent(t *testing.T, title, body, fp string) {
	t.Helper()
	if !strings.HasPrefix(title, "[dalgona-failure:v1:"+fp+"] dalgona ") {
		t.Fatalf("title = %q", title)
	}
	if !strings.Contains(body, "Automated failure report prepared by dalgona.") {
		t.Fatalf("body missing header: %q", body)
	}
	if !strings.Contains(body, `"status":"ready"`) {
		t.Fatalf("body missing failing step: %q", body)
	}
}

func requireFiledReceipt(t *testing.T, home string, number int64) {
	t.Helper()
	state := readState(t, home)
	if len(state.Receipts) != 1 || state.Receipts[0].State != receiptFiled || state.Receipts[0].IssueNumber != number {
		t.Fatalf("receipts = %+v", state.Receipts)
	}
}

func (f *fakeServer) postCount() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.posts)
}

func TestConsentGateStopsAfterWithdrawal(t *testing.T) {
	fake := &fakeServer{}
	fake.onSearch = emptySearch
	fake.onCreate = func(_ *http.Request, _ []byte) (int, string) {
		return http.StatusCreated, createdPayload(1, "t", "b")
	}
	server := httptest.NewServer(fake.handler())
	defer server.Close()
	home := t.TempDir()
	clock := &manualClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	source := &stubSource{access: "token", expires: time.Now().Add(time.Hour)}
	if err := WriteConsent(home, "gh", clock.now); err != nil {
		t.Fatalf("WriteConsent: %v", err)
	}
	client, err := NewReporterClient(server.URL, server.Client(), "test", source)
	if err != nil {
		t.Fatalf("NewReporterClient: %v", err)
	}
	r, err := Open(context.Background(), Options{
		Home:          home,
		Client:        client,
		Credentials:   source,
		Eligibility:   Eligibility{Origin: failure.OriginTest, Test: true},
		Now:           clock.Now,
		Notice:        io.Discard,
		ConsentMethod: "gh",
	})
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	defer func() { _ = r.Close(context.Background()) }()
	fp := "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	if err = r.Enqueue(context.Background(), testRecord(fp)); err != nil {
		t.Fatalf("Enqueue: %v", err)
	}
	waitFor(t, "create", func() bool { return fake.createCount() == 1 })
	if err = Disable(home); err != nil {
		t.Fatalf("Disable: %v", err)
	}
	fresh := "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
	err = errors.Join(
		r.Enqueue(context.Background(), testRecord(fresh)),
		r.Drain(context.Background()),
	)
	if err != nil {
		t.Fatalf("post-withdrawal reporting: %v", err)
	}
	if got := fake.createCount(); got != 1 {
		t.Fatalf("createCount after withdrawal = %d, want 1", got)
	}
	if got := r.queueLen(); got != 0 {
		t.Fatalf("queue length after withdrawal = %d, want 0", got)
	}
}
