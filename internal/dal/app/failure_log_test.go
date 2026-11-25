package app

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/failure"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/llmstub"
)

const failureProbeKey = "failure-probe-key"

const failureProbeScript = `{"models":[{"id":"probe-model"}],"turns":[
{"id":"t-call","when":{"user":"probe"},"steps":[{"tool_call":{"id":"call-probe","name":"probe","arguments":{}}},{"finish":"tool_calls"}]},
{"id":"t-done","when":{"tool_result":"call-probe"},"steps":[{"text":"done"},{"finish":"stop"}]}]}`

type brokenHelperTool struct{}

func (brokenHelperTool) Spec() tool.Spec {
	return tool.Spec{Name: "probe", Description: "probe", Schema: jsontext.Value(`{"type":"object"}`)}
}

func (brokenHelperTool) Class() tool.Class { return tool.ClassRead }

func (brokenHelperTool) Run(context.Context, tool.Call, tool.Env) (tool.Result, error) {
	return tool.Result{Failure: tool.FailureInternal}, errors.New("probe helper exited before it answered")
}

type deliveredRecords struct {
	mu      sync.Mutex
	records []failure.Record
}

func (d *deliveredRecords) take(record failure.Record) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.records = append(d.records, record)
}

func (d *deliveredRecords) all() []failure.Record {
	d.mu.Lock()
	defer d.mu.Unlock()
	return append([]failure.Record(nil), d.records...)
}

func TestHostRecordsToolFailuresForBothProducts(t *testing.T) {
	t.Run(string(config.ProductDal), func(t *testing.T) {
		checkHostRecordsToolFailure(t, config.ProductDal)
	})
	t.Run(string(config.ProductDalgona), func(t *testing.T) {
		checkHostRecordsToolFailure(t, config.ProductDalgona)
	})
}

func checkHostRecordsToolFailure(t *testing.T, product config.Product) {
	t.Helper()
	home, delivered := probeFailingTool(t, product)
	store, err := failure.Open(t.Context(), failure.Options{Home: home})
	if err != nil {
		t.Fatal(err)
	}
	stored, err := store.List(t.Context(), failure.Query{})
	if err != nil {
		t.Fatal(err)
	}
	record := checkFailureDelivery(t, stored, delivered.all())
	checkFailureProduct(t, record, product)
}

func checkFailureDelivery(t *testing.T, stored, sent []failure.Record) failure.Record {
	t.Helper()
	if len(stored) != 1 || len(sent) != 1 || stored[0].ID != sent[0].ID {
		t.Fatalf("stored %d and delivered %d records, want the same single record", len(stored), len(sent))
	}
	return stored[0]
}

func checkFailureProduct(t *testing.T, record failure.Record, product config.Product) {
	t.Helper()
	if record.Product != string(product) || record.Class != failure.ClassTool || record.Tool != "probe" {
		t.Fatalf("record = %+v, want a %s tool record for probe", record, product)
	}
	if record.Reportable != (product == config.ProductDalgona) {
		t.Fatalf("%s record reportable = %v", product, record.Reportable)
	}
}

func probeFailingTool(t *testing.T, product config.Product) (string, *deliveredRecords) {
	t.Helper()
	clearProviderKeys(t)
	script, err := llmstub.Parse("failure-probe", []byte(failureProbeScript))
	if err != nil {
		t.Fatal(err)
	}
	server := httptest.NewServer(llmstub.NewHandler(script, llmstub.Options{Key: failureProbeKey}))
	t.Cleanup(server.Close)
	t.Setenv("FAILURE_PROBE_KEY", failureProbeKey)
	home, cwd := t.TempDir(), t.TempDir()
	t.Setenv("DALGONA_HOME", home)
	t.Setenv("DAL_HOME", home)
	body := "[providers.local]\nbase_url = \"" + server.URL + "/v1\"\napi_key_env = \"FAILURE_PROBE_KEY\"\n"
	if writeErr := os.WriteFile(filepath.Join(home, "config.toml"), []byte(body), 0o600); writeErr != nil {
		t.Fatal(writeErr)
	}
	cfg, err := config.Load(product, cwd, config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	delivered := &deliveredRecords{}
	batteries := func(context.Context, Base) (Batteries, error) {
		return Batteries{
			Tools:       func(context.Context, string) []tool.Tool { return []tool.Tool{brokenHelperTool{}} },
			FailureSink: delivered.take,
		}, nil
	}
	host, closeHost, err := NewHost(t.Context(), Options{Product: product, Cwd: cwd, Config: &cfg, Batteries: batteries})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = closeHost() })
	thread, err := host.Start(t.Context(), agent.ThreadSpec{Cwd: cwd, Model: "local/probe-model", Approval: config.ApprovalYolo})
	if err != nil {
		t.Fatal(err)
	}
	updates := thread.Subscribe()
	t.Cleanup(func() { thread.Unsubscribe(updates) })
	if err = thread.Submit(t.Context(), agent.Input{Text: "probe"}); err != nil {
		t.Fatal(err)
	}
	awaitTurnCompleted(t, updates)
	return home, delivered
}
