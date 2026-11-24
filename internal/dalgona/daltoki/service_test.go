package daltoki

import (
	"context"
	json "encoding/json/v2"
	"errors"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/journal"
	"github.com/gosuda/dalgona/internal/dalgona/github"
)

func recordsOfKind(t *testing.T, home, kind string) []serviceRecord {
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
		if kind == "" || record.Kind == kind {
			records = append(records, record)
		}
		return nil
	})
	if err != nil {
		t.Fatalf("replay service journal: %v", err)
	}
	return records
}

func barrierRecords(t *testing.T, home string) []serviceRecord {
	t.Helper()
	return recordsOfKind(t, home, kindControlChanged)
}

type stubCredential struct{}

func (stubCredential) Token(ctx context.Context, req github.CredentialRequest) (github.Token, error) {
	return github.Token{Value: "test-token"}, nil
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
		Repositories:            []github.Repository{{Owner: "octo", Name: "repo"}},
		PollInterval:            60 * time.Second,
		Concurrency:             2,
		JobsPerDay:              20,
		Model:                   "model",
		Profile:                 "hashline-light",
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
		Model: "model", Profile: "hashline-light",
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
		{"profile caps", func(c *config.DaltokiConfig) { c.Profile = "DalToki" }, "daltoki: invalid profile"},
		{"profile spaces", func(c *config.DaltokiConfig) { c.Profile = "dal toki" }, "daltoki: invalid profile"},
		{"profile unknown", func(c *config.DaltokiConfig) { c.Profile = "no-such-style" }, "daltoki: unknown profile \"no-such-style\""},
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
	svc, err := New(context.Background(), enabledConfig(), fakeDeps(t, t.TempDir()))
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
	failed, err := New(context.Background(), enabledConfig(), fakeDeps(t, t.TempDir()))
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
	if closeErr := failed.Close(context.Background()); closeErr != nil {
		t.Fatalf("close failed-recovery service: %v", closeErr)
	}
}

func TestRetirePreservesState(t *testing.T) {
	home := t.TempDir()
	deps := fakeDeps(t, home)
	graphs, _ := deps.Graphs.(*fakeGraphs)
	worktrees, _ := deps.Worktrees.(*fakeWorktrees)
	svc, err := New(context.Background(), enabledConfig(), deps)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	svc.Attach(nil)
	svc.StartAfterRecovery(nil)
	runUntilClosed(t, svc)
	assertShutdownBarrier(t, home)
	if graphs.cancelled() != 1 || worktrees.closedCount() != 1 {
		t.Fatalf("graphs=%d worktrees=%d", graphs.cancelled(), worktrees.closedCount())
	}
	if closeErr := svc.Close(context.Background()); closeErr != nil {
		t.Fatalf("second Close: %v", closeErr)
	}
	if got := len(barrierRecords(t, home)); got != 1 {
		t.Fatalf("second Close recorded again: %d", got)
	}

	blockedHome := t.TempDir()
	blocked, err := New(context.Background(), enabledConfig(), fakeDeps(t, blockedHome))
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	blocked.workers.Add(1)
	timeout, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	if closeErr := blocked.Close(timeout); closeErr == nil {
		t.Fatal("expected timeout error for owned worker")
	}
	blocked.workers.Done()
	waitForJournalFree(t, blockedHome)
}

func waitForJournalFree(t *testing.T, home string) {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for {
		j, err := journal.OpenShared(serviceJournalDir(home))
		if err == nil {
			_ = j.Close()
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("service journal stayed locked: %v", err)
		}
		time.Sleep(50 * time.Millisecond)
	}
}
