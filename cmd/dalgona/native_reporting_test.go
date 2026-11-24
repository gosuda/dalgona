package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/failure"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/report"
)

type unmaskedText struct{}

func (unmaskedText) MaskWithError(text string) (string, error) { return text, nil }

type consentReceiptResult struct {
	SchemaVersion int    `json:"schemaVersion"`
	Time          string `json:"time"`
	Method        string `json:"method"`
	TextHash      string `json:"textHash"`
}

type restrictionResult struct {
	Kind    string `json:"kind"`
	Until   string `json:"until"`
	Message string `json:"message"`
}

type reportingResult struct {
	Method         string                `json:"method"`
	Enabled        bool                  `json:"enabled"`
	Available      bool                  `json:"available"`
	ConsentText    string                `json:"consentText"`
	TextHash       string                `json:"textHash"`
	ConsentReceipt *consentReceiptResult `json:"consentReceipt"`
	Authorization  string                `json:"authorization"`
	Restriction    *restrictionResult    `json:"restriction"`
	QueueCount     int                   `json:"queueCount"`
	UncertainCount int                   `json:"uncertainCount"`
}

type enableResult struct {
	ConsentReceipt *consentReceiptResult `json:"consentReceipt"`
	Authorization  string                `json:"authorization"`
}

type reportingRig struct {
	*nativeRig
	reloads  []bool
	onReload func()
}

func newReportingRig(t *testing.T, before func(home string)) *reportingRig {
	t.Helper()
	rig := &reportingRig{}
	rig.nativeRig = newNativeRig(t, before, func(home string) rpcserver.ServeOptions {
		reload := func() {
			cfg := failureReportsConfig(t)
			rig.reloads = append(rig.reloads, cfg.Enabled && report.ConsentValid(home, cfg.Auth))
			if rig.onReload != nil {
				rig.onReload()
			}
		}
		return rpcserver.ServeOptions{Extensions: reportingExtensions(), OperationExtensions: reportingOperations(reload)}
	})
	return rig
}

func (r *reportingRig) lastReloadActive(t *testing.T) bool {
	t.Helper()
	if len(r.reloads) == 0 {
		t.Fatal("the daemon reporter was never reloaded")
	}
	return r.reloads[len(r.reloads)-1]
}

func failureReportsConfig(t *testing.T) config.FailureReportsConfig {
	t.Helper()
	cfg, err := config.Load(config.ProductDalgona, "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	return cfg.FailureReports
}

func (r *nativeRig) reporting(t *testing.T) reportingResult {
	t.Helper()
	var state reportingResult
	if err := r.peer.Call(t.Context(), methodReportingRead, map[string]any{}, &state); err != nil {
		t.Fatal(err)
	}
	return state
}

func (r *nativeRig) enable(t *testing.T, textHash string) enableResult {
	t.Helper()
	var result enableResult
	params := map[string]any{"method": "app", "textHash": textHash, "confirmation": "yes", "operationId": rpc.NewOperationID()}
	if err := r.peer.Call(t.Context(), methodReportingEnable, params, &result); err != nil {
		t.Fatalf("enable reporting: %v", err)
	}
	return result
}

func openTestReporter(t *testing.T, home string) *report.Reporter {
	t.Helper()
	reporter, err := report.Open(t.Context(), report.Options{
		Home: home, ConsentMethod: "app", Eligibility: report.Eligibility{Origin: failure.OriginMain},
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = reporter.Close(context.Background()) })
	return reporter
}

func enqueueAndCount(t *testing.T, reporter *report.Reporter, home string) int {
	t.Helper()
	record, err := failure.Build(t.Context(), failure.Input{
		Product: "dalgona", Version: "1.0", Class: failure.ClassTool, Tool: "exec", Err: errors.New("exec tool failed"), Origin: failure.OriginMain,
	}, unmaskedText{})
	if err != nil {
		t.Fatal(err)
	}
	if err := reporter.Enqueue(t.Context(), record); err != nil {
		t.Fatalf("enqueue: %v", err)
	}
	return report.Inspect(home, "app", time.Now()).QueueCount
}

func sha256Hex(text string) string {
	sum := sha256.Sum256([]byte(text))
	return hex.EncodeToString(sum[:])
}

func TestReportingEnableRecordsConsentBeforeAnyReport(t *testing.T) {
	rig := newReportingRig(t, func(string) {})
	reporter := openTestReporter(t, rig.home)
	if queued := enqueueAndCount(t, reporter, rig.home); queued != 0 {
		t.Fatalf("queue before consent = %d, want 0", queued)
	}
	state := requireInitialReportingState(t, rig)
	requireEnableRefusals(t, rig, state)
	enabled := requireReportingEnabled(t, rig, state)
	requireEnabledReportingState(t, rig, reporter, enabled)
	requireGhReportingSwitch(t, rig, state)
}

func requireInitialReportingState(t *testing.T, rig *reportingRig) reportingResult {
	t.Helper()
	state := rig.reporting(t)
	if state.Method != "app" || state.Enabled || state.Available || state.ConsentReceipt != nil || state.ConsentText == "" {
		t.Fatalf("initial reporting state = %+v", state)
	}
	if state.TextHash != sha256Hex(state.ConsentText) {
		t.Fatalf("text hash %s does not hash the consent text", state.TextHash)
	}
	return state
}

func requireEnableRefusals(t *testing.T, rig *reportingRig, state reportingResult) {
	t.Helper()
	refusals := []struct {
		params map[string]any
		kind   rpc.ErrorKind
	}{
		{map[string]any{"method": "app", "textHash": state.TextHash, "confirmation": "Yes"}, rpc.ErrorPermissionDenied},
		{map[string]any{"method": "app", "textHash": sha256Hex("older consent text"), "confirmation": "yes"}, rpc.ErrorStaleRevision},
		{map[string]any{"method": "email", "textHash": state.TextHash, "confirmation": "yes"}, rpc.ErrorInvalidParams},
	}
	for _, refusal := range refusals {
		refusal.params["operationId"] = rpc.NewOperationID()
		data := rig.refused(t, methodReportingEnable, refusal.params, refusal.kind)
		if refusal.kind == rpc.ErrorStaleRevision && data.CurrentRevision != state.TextHash {
			t.Fatalf("stale revision names %q, want %q", data.CurrentRevision, state.TextHash)
		}
	}
	if after := rig.reporting(t); after.ConsentReceipt != nil || failureReportsConfig(t).Enabled {
		t.Fatalf("refused enable left consent %+v or enabled config", after.ConsentReceipt)
	}
}

func requireReportingEnabled(t *testing.T, rig *reportingRig, state reportingResult) enableResult {
	t.Helper()
	enabled := rig.enable(t, state.TextHash)
	if !rig.lastReloadActive(t) {
		t.Fatal("the daemon reporter reloaded before the receipt and configuration enabled reporting")
	}
	receipt := enabled.ConsentReceipt
	if receipt == nil || receipt.SchemaVersion != 1 || receipt.Method != "app" || receipt.TextHash != state.TextHash || receipt.Time == "" {
		t.Fatalf("enable receipt = %+v", receipt)
	}
	if enabled.Authorization == "" {
		t.Fatal("enable result has no authorization state")
	}
	return enabled
}

func requireEnabledReportingState(t *testing.T, rig *reportingRig, reporter *report.Reporter, enabled enableResult) {
	t.Helper()
	if cfg := failureReportsConfig(t); !cfg.Enabled || cfg.Auth != "app" {
		t.Fatalf("failure reports config = %+v, want enabled app", cfg)
	}
	if after := rig.reporting(t); !after.Enabled || after.ConsentReceipt == nil || *after.ConsentReceipt != *enabled.ConsentReceipt {
		t.Fatalf("reporting state after enable = %+v", after)
	}
	if queued := enqueueAndCount(t, reporter, rig.home); queued != 1 {
		t.Fatalf("queue after consent = %d, want 1", queued)
	}
}

func requireGhReportingSwitch(t *testing.T, rig *reportingRig, state reportingResult) {
	t.Helper()
	requireAppDisabledByGhReceipt(t, rig)
	requireGhReportingEnabled(t, rig, state)
}

func requireAppDisabledByGhReceipt(t *testing.T, rig *reportingRig) {
	t.Helper()
	if err := report.WriteConsent(rig.home, "gh", time.Now()); err != nil {
		t.Fatal(err)
	}
	if after := rig.reporting(t); after.Enabled || after.Method != "app" {
		t.Fatalf("app reporting state after a gh receipt replaced the app receipt = %+v", after)
	}
}

func requireGhReportingEnabled(t *testing.T, rig *reportingRig, state reportingResult) {
	t.Helper()
	var ghState reportingResult
	if err := rig.peer.Call(t.Context(), methodReportingRead, map[string]any{"method": "gh"}, &ghState); err != nil {
		t.Fatal(err)
	}
	requireGhPreEnableState(t, ghState, state)
	params := map[string]any{"method": "gh", "textHash": ghState.TextHash, "confirmation": "yes", "operationId": rpc.NewOperationID()}
	if err := rig.peer.Call(t.Context(), methodReportingEnable, params, nil); err != nil {
		t.Fatalf("enable gh reporting: %v", err)
	}
	if after := rig.reporting(t); !after.Enabled || after.Method != "gh" || after.ConsentReceipt == nil || after.ConsentReceipt.Method != "gh" {
		t.Fatalf("reporting state after enabling gh = %+v", after)
	}
}

func requireGhPreEnableState(t *testing.T, ghState, state reportingResult) {
	t.Helper()
	if ghState.Enabled || ghState.TextHash == state.TextHash || ghState.TextHash != sha256Hex(ghState.ConsentText) {
		t.Fatalf("gh reporting state before enabling gh = %+v", ghState)
	}
}

func TestReportingEnableStaysOffWhenTheReceiptCannotBeWritten(t *testing.T) {
	rig := newReportingRig(t, func(home string) {
		if err := os.WriteFile(filepath.Join(home, "failure-reporting"), nil, 0o600); err != nil {
			t.Fatal(err)
		}
	})
	state := rig.reporting(t)
	params := map[string]any{"method": "app", "textHash": state.TextHash, "confirmation": "yes", "operationId": rpc.NewOperationID()}
	if data := rig.refused(t, methodReportingEnable, params, rpc.ErrorUnavailable); data.Stage != "consent" {
		t.Fatalf("failed enable stage = %q, want consent", data.Stage)
	}
	if failureReportsConfig(t).Enabled || rig.reporting(t).Enabled {
		t.Fatal("reporting enabled without a stored receipt")
	}
}

func TestReportingDisableDeletesQueueAfterAnAdmittedEnqueue(t *testing.T) {
	rig := newReportingRig(t, func(string) {})
	rig.enable(t, rig.reporting(t).TextHash)
	arrived, finish := make(chan struct{}), make(chan struct{})
	unblock := sync.OnceFunc(func() { close(finish) })
	defer unblock()
	reporter := pausedEnqueueReporter(t, rig.home, arrived, finish)
	record, err := failure.Build(t.Context(), failure.Input{
		Product: "dalgona", Version: "1.0", Class: failure.ClassTool, Tool: "exec", Err: errors.New("exec tool failed"), Origin: failure.OriginMain,
	}, unmaskedText{})
	if err != nil {
		t.Fatal(err)
	}
	enqueued := make(chan error, 1)
	go func() { enqueued <- reporter.Enqueue(t.Context(), record) }()
	<-arrived
	rig.onReload = func() {
		unblock()
		if err := errors.Join(<-enqueued, reporter.Close(context.Background())); err != nil {
			t.Error(err)
		}
	}
	requireDisableResult(t, rig)
	if got := rig.reporting(t).QueueCount; got != 0 {
		t.Fatalf("queue contains %d reports after disable reported deletion", got)
	}
}

func pausedEnqueueReporter(t *testing.T, home string, arrived chan struct{}, finish <-chan struct{}) *report.Reporter {
	t.Helper()
	pause := sync.OnceFunc(func() {
		close(arrived)
		<-finish
	})
	reporter, err := report.Open(t.Context(), report.Options{
		Home: home, ConsentMethod: "app", Eligibility: report.Eligibility{Origin: failure.OriginMain},
		Now: func() time.Time {
			pause()
			return time.Now()
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = reporter.Close(context.Background()) })
	return reporter
}

func TestReportingDisableStopsReportsAndKeepsTheLatch(t *testing.T) {
	rig := newReportingRig(t, func(home string) {
		dir := filepath.Join(home, "failure-reporting")
		if err := os.Mkdir(dir, 0o700); err != nil {
			t.Fatal(err)
		}
		state := `{"receipts":[{"fingerprint":"` + sha256Hex("earlier") + `","state":"uncertain"}],"stoppedStatus":403}`
		if err := os.WriteFile(filepath.Join(dir, "state.json"), []byte(state), 0o600); err != nil {
			t.Fatal(err)
		}
	})
	rig.enable(t, rig.reporting(t).TextHash)
	reporter := openTestReporter(t, rig.home)
	if queued := enqueueAndCount(t, reporter, rig.home); queued != 1 {
		t.Fatalf("queue while enabled = %d, want 1", queued)
	}
	requireDisableResult(t, rig)
	requireDisabledLatchState(t, rig, reporter)
}

func requireDisableResult(t *testing.T, rig *reportingRig) {
	t.Helper()
	var result jsontext.Value
	if err := rig.peer.Call(t.Context(), methodReportingDisable, map[string]any{"operationId": rpc.NewOperationID()}, &result); err != nil {
		t.Fatal(err)
	}
	if rig.lastReloadActive(t) {
		t.Fatal("the daemon reporter reloaded while reporting was still enabled")
	}
	want := jsontext.Value(`{"enabled":false,"queuedReportsDeleted":true,"publishedIssuesDeleted":false}`)
	if err := result.Canonicalize(); err != nil || string(result) != string(mustCanonical(t, want)) {
		t.Fatalf("disable result = %s, want %s", result, want)
	}
}

func requireDisabledLatchState(t *testing.T, rig *reportingRig, reporter *report.Reporter) {
	t.Helper()
	state := rig.reporting(t)
	wantLatch := restrictionResult{Kind: "stopped", Message: "failure reporting stopped after HTTP 403; review access before enabling again"}
	if state.Enabled || state.ConsentReceipt != nil || state.QueueCount != 0 || state.UncertainCount != 1 || state.Restriction == nil || *state.Restriction != wantLatch {
		t.Fatalf("reporting state after disable = %+v restriction %+v", state, state.Restriction)
	}
	if failureReportsConfig(t).Enabled {
		t.Fatal("failure reports config still enabled")
	}
	if queued := enqueueAndCount(t, reporter, rig.home); queued != 0 {
		t.Fatalf("queue after disable = %d, want 0", queued)
	}
	rig.refused(t, methodReportingDisable, map[string]any{}, rpc.ErrorInvalidParams)
}

func mustCanonical(t *testing.T, value jsontext.Value) jsontext.Value {
	t.Helper()
	canonical := append(jsontext.Value(nil), value...)
	if err := canonical.Canonicalize(); err != nil {
		t.Fatal(err)
	}
	return canonical
}
