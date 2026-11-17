package daltoki

import (
	"context"
	json "encoding/json/v2"
	"errors"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/journal"
	"github.com/gosuda/dalgona/internal/dalgona/github"
)

func newStubGitHub(t *testing.T) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte("[]"))
	}))
	t.Cleanup(server.Close)
	return server
}

func barrierRecords(t *testing.T, home string) []serviceRecord {
	t.Helper()
	j, err := journal.OpenShared(serviceJournalDir(home))
	if err != nil {
		t.Fatalf("open service journal: %v", err)
	}
	defer func() { _ = j.Close() }()
	var records []serviceRecord
	err = j.Replay(journalSession, func(_ uint64, ev []byte) error {
		var record serviceRecord
		if decodeErr := json.Unmarshal(ev, &record); decodeErr != nil {
			return decodeErr
		}
		if record.Kind == kindControlChanged {
			records = append(records, record)
		}
		return nil
	})
	if err != nil {
		t.Fatalf("replay service journal: %v", err)
	}
	return records
}

type fakeGraphs struct {
	mu     sync.Mutex
	cancel int
}

func (f *fakeGraphs) CancelAll(ctx context.Context) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.cancel++
	return nil
}

type fakeWorktrees struct {
	mu     sync.Mutex
	closed int
}

func (f *fakeWorktrees) Close() error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.closed++
	return nil
}

type stubCredential struct{}

func (stubCredential) Token(ctx context.Context, req github.CredentialRequest) (github.Token, error) {
	return github.Token{Value: "test-token"}, nil
}

func testClient(t *testing.T) *github.Client {
	t.Helper()
	client, err := github.NewClient(github.ClientConfig{
		BaseURL:      newStubGitHub(t).URL,
		Repositories: []github.Repository{{Owner: "octo", Name: "repo"}},
		Policy:       "bot",
	}, stubCredential{}, nil)
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	return client
}

func testStore(t *testing.T) *auth.Store {
	t.Helper()
	store, err := auth.NewWithPatterns(t.TempDir(), nil)
	if err != nil {
		t.Fatalf("NewWithPatterns: %v", err)
	}
	return store
}

func enabledConfig() Config {
	return Config{
		Enabled:                 true,
		AppID:                   42,
		PrivateKeyFile:          "/nonexistent/key.pem",
		PollInterval:            60 * time.Second,
		Concurrency:             2,
		JobsPerDay:              20,
		Model:                   "model",
		Profile:                 "daltoki",
		JobTokenLimit:           100000,
		DailyTokenLimit:         500000,
		JobCostLimitUSDMicros:   5000000,
		DailyCostLimitUSDMicros: 20000000,
	}
}

func okUnmask(value string) (string, error) { return "resolved-" + value, nil }

func TestDisabledDoesNoIO(t *testing.T) {
	cfg, err := ParseConfig(config.DaltokiConfig{}, nil)
	if err != nil {
		t.Fatalf("ParseConfig disabled: %v", err)
	}
	if cfg.Enabled {
		t.Fatal("disabled config parsed as enabled")
	}
	svc, err := New(context.Background(), cfg, Dependencies{Home: t.TempDir()})
	if err != nil {
		t.Fatalf("New disabled: %v", err)
	}
	svc.Attach(nil)
	svc.StartAfterRecovery(nil)
	if err := svc.Run(context.Background()); err != nil {
		t.Fatalf("Run disabled: %v", err)
	}
	status := svc.Status()
	if status.Enabled || status.Running || status.LastError != "" {
		t.Fatalf("status = %+v", status)
	}
	if err := svc.Close(context.Background()); err != nil {
		t.Fatalf("Close disabled: %v", err)
	}
}

func TestConfigBounds(t *testing.T) {
	valid := config.DaltokiConfig{
		Enabled: true, AppID: 7, PrivateKeyFile: "/keys/app.pem",
		Repositories:        []string{"Gosuda/Dalgona", "octo/repo"},
		PollIntervalSeconds: 60, Concurrency: 2, JobsPerDay: 20,
		Model: "model", Profile: "daltoki",
		JobTokenLimit: 100000, DailyTokenLimit: 500000,
		JobCostLimitUSDMicros: 5000000, DailyCostLimitUSDMicros: 20000000,
	}
	cfg, err := ParseConfig(valid, okUnmask)
	if err != nil {
		t.Fatalf("ParseConfig valid: %v", err)
	}
	if len(cfg.Repositories) != 2 || cfg.Repositories[0].Owner != "gosuda" {
		t.Fatalf("repositories not canonicalized: %+v", cfg.Repositories)
	}
	cases := []struct {
		name   string
		mutate func(*config.DaltokiConfig)
		want   string
	}{
		{"app id", func(c *config.DaltokiConfig) { c.AppID = 0 }, "daltoki: invalid app_id"},
		{"key file", func(c *config.DaltokiConfig) { c.PrivateKeyFile = "" }, "daltoki: invalid private_key_file"},
		{"model", func(c *config.DaltokiConfig) { c.Model = "" }, "daltoki: invalid model"},
		{"profile", func(c *config.DaltokiConfig) { c.Profile = "" }, "daltoki: invalid profile"},
		{"no repositories", func(c *config.DaltokiConfig) { c.Repositories = nil }, "daltoki: invalid repositories"},
		{"bad repository", func(c *config.DaltokiConfig) { c.Repositories = []string{"not a repo!"} }, "daltoki: invalid repositories"},
		{"dot repository", func(c *config.DaltokiConfig) { c.Repositories = []string{"./x"} }, "daltoki: invalid repositories"},
		{"duplicate repository", func(c *config.DaltokiConfig) { c.Repositories = []string{"a/b", "A/B"} }, "daltoki: invalid repositories"},
		{"poll low", func(c *config.DaltokiConfig) { c.PollIntervalSeconds = 59 }, "daltoki: invalid poll_interval_seconds"},
		{"poll high", func(c *config.DaltokiConfig) { c.PollIntervalSeconds = 3601 }, "daltoki: invalid poll_interval_seconds"},
		{"concurrency", func(c *config.DaltokiConfig) { c.Concurrency = 5 }, "daltoki: invalid concurrency"},
		{"jobs per day", func(c *config.DaltokiConfig) { c.JobsPerDay = 0 }, "daltoki: invalid jobs_per_day"},
		{"job tokens", func(c *config.DaltokiConfig) { c.JobTokenLimit = 999 }, "daltoki: invalid job_token_limit"},
		{"daily tokens below job", func(c *config.DaltokiConfig) { c.DailyTokenLimit = 99999 }, "daltoki: invalid daily_token_limit"},
		{"job cost", func(c *config.DaltokiConfig) { c.JobCostLimitUSDMicros = 0 }, "daltoki: invalid job_cost_limit_usd_micros"},
		{"daily cost below job", func(c *config.DaltokiConfig) { c.DailyCostLimitUSDMicros = 1 }, "daltoki: invalid daily_cost_limit_usd_micros"},
		{"literal secret", func(c *config.DaltokiConfig) { c.WebhookSecretRef = "literal" }, "daltoki: invalid webhook_secret_ref"},
		{"composite secret", func(c *config.DaltokiConfig) { c.WebhookSecretRef = "x{{secret:a}}y" }, "daltoki: invalid webhook_secret_ref"},
		{"unresolved secret", func(c *config.DaltokiConfig) { c.WebhookSecretRef = "{{secret:missing}}" }, "daltoki: invalid webhook_secret_ref"},
		{"partial relay", func(c *config.DaltokiConfig) { c.RelayURL = "https://relay.example.test" }, "daltoki: relay configuration is incomplete"},
		{"http relay", func(c *config.DaltokiConfig) {
			c.RelayURL = "http://relay.example.test"
			c.RelayTokenRef = "{{secret:relay}}"
		}, "daltoki: invalid relay_url"},
		{"relay query", func(c *config.DaltokiConfig) {
			c.RelayURL = "https://relay.example.test/?x=1"
			c.RelayTokenRef = "{{secret:relay}}"
		}, "daltoki: invalid relay_url"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			raw := valid
			raw.Repositories = append([]string(nil), valid.Repositories...)
			tc.mutate(&raw)
			unmask := okUnmask
			if tc.name == "unresolved secret" {
				unmask = func(string) (string, error) { return "", errors.New("missing") }
			}
			_, err := ParseConfig(raw, unmask)
			if err == nil || err.Error() != tc.want {
				t.Fatalf("err = %v, want %q", err, tc.want)
			}
		})
	}
}

func runUntilClosed(t *testing.T, svc *Service) {
	t.Helper()
	runErr := make(chan error, 1)
	go func() { runErr <- svc.Run(context.Background()) }()
	time.Sleep(50 * time.Millisecond)
	if got := svc.Status(); !got.Enabled || !got.Running {
		t.Fatalf("status = %+v", got)
	}
	if closeErr := svc.Close(context.Background()); closeErr != nil {
		t.Fatalf("Close: %v", closeErr)
	}
	select {
	case runResult := <-runErr:
		if runResult != nil {
			t.Fatalf("Run: %v", runResult)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Run did not return after Close")
	}
}

func assertShutdownBarrier(t *testing.T, home string) {
	t.Helper()
	records := barrierRecords(t, home)
	if len(records) != 1 {
		t.Fatalf("control records = %d", len(records))
	}
	barrier := records[0]
	if barrier.Version != 1 || barrier.Control == nil || !barrier.Control.Shutdown {
		t.Fatalf("barrier = %+v", barrier)
	}
}

func TestStartsAfterRecovery(t *testing.T) {
	svc, err := New(context.Background(), enabledConfig(), Dependencies{Home: t.TempDir(), Client: testClient(t), Auth: testStore(t)})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	var order []string
	svc.Attach(nil)
	order = append(order, "attach")
	svc.StartAfterRecovery(nil)
	order = append(order, "start")
	if got := svc.Status().LastError; got != "" {
		t.Fatalf("LastError = %q", got)
	}
	order = append(order, "close")
	runUntilClosed(t, svc)
	if len(order) != 3 || order[0] != "attach" || order[1] != "start" || order[2] != "close" {
		t.Fatalf("order = %v", order)
	}
	assertFailedRecoveryRefusesRun(t)
}

func assertFailedRecoveryRefusesRun(t *testing.T) {
	t.Helper()
	failed, err := New(context.Background(), enabledConfig(), Dependencies{Home: t.TempDir(), Client: testClient(t), Auth: testStore(t)})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	failed.StartAfterRecovery(errors.New("boom"))
	if got := failed.Status().LastError; got != "daltoki: host recovery failed" {
		t.Fatalf("LastError = %q", got)
	}
	if runErr := failed.Run(context.Background()); runErr == nil || runErr.Error() != "daltoki: host recovery failed" {
		t.Fatalf("Run after failed recovery = %v", runErr)
	}
}

func TestRetirePreservesState(t *testing.T) {
	home := t.TempDir()
	graphs := &fakeGraphs{}
	worktrees := &fakeWorktrees{}
	svc, err := New(context.Background(), enabledConfig(), Dependencies{
		Home: home, Graphs: graphs, Worktrees: worktrees, Client: testClient(t), Auth: testStore(t),
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	svc.Attach(nil)
	svc.StartAfterRecovery(nil)
	runUntilClosed(t, svc)
	assertShutdownBarrier(t, home)
	if graphs.cancel != 1 || worktrees.closed != 1 {
		t.Fatalf("graphs=%d worktrees=%d", graphs.cancel, worktrees.closed)
	}
	if closeErr := svc.Close(context.Background()); closeErr != nil {
		t.Fatalf("second Close: %v", closeErr)
	}
	if got := len(barrierRecords(t, home)); got != 1 {
		t.Fatalf("second Close recorded again: %d", got)
	}

	blocked, err := New(context.Background(), enabledConfig(), Dependencies{Home: t.TempDir(), Client: testClient(t), Auth: testStore(t)})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	blocked.workers.Add(1)
	timeout, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	if closeErr := blocked.Close(timeout); closeErr == nil {
		t.Fatal("expected timeout error for owned worker")
	}
}
