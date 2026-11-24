package agent

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/failure"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

var (
	sampleSecret = "AKIA" + strings.Repeat("Q", 16)
	secretNote   = `{"note":"` + sampleSecret + `"}`
)

type recordSink struct {
	mu      sync.Mutex
	records []failure.Record
}

func (s *recordSink) take(record failure.Record) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.records = append(s.records, record)
}

func (s *recordSink) all() []failure.Record {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]failure.Record(nil), s.records...)
}

type failureLogRig struct {
	store *failure.Store
	sink  *recordSink
	dir   string
}

func withFailureLog(rig *failureLogRig) func(*harness) {
	return func(h *harness) {
		masker, err := auth.Load(h.home)
		if err != nil {
			h.t.Fatal(err)
		}
		store, err := failure.Open(h.t.Context(), failure.Options{Home: h.home, Product: "dalgona", Masker: masker})
		if err != nil {
			h.t.Fatal(err)
		}
		rig.store, rig.sink, rig.dir = store, &recordSink{}, filepath.Join(h.home, "failures")
		h.cfg.Auth, h.cfg.Failures, h.cfg.FailureSink = masker, store, rig.sink.take
	}
}

func withExtraTool(item tool.Tool) func(*harness) {
	return func(h *harness) { h.extraTools = append(h.extraTools, item) }
}

func (rig *failureLogRig) onlyRecord(t *testing.T) failure.Record {
	t.Helper()
	stored, err := rig.store.List(t.Context(), failure.Query{})
	if err != nil {
		t.Fatal(err)
	}
	delivered := rig.sink.all()
	if len(stored) != 1 || len(delivered) != 1 {
		t.Fatalf("stored %d records and delivered %d, want exactly one of each", len(stored), len(delivered))
	}
	if stored[0].ID != delivered[0].ID || stored[0].Fingerprint != delivered[0].Fingerprint {
		t.Fatalf("delivered record %s is not the committed record %s", delivered[0].ID, stored[0].ID)
	}
	requireNoRawBytes(t, rig.dir, sampleSecret)
	return stored[0]
}

func (rig *failureLogRig) requireNone(t *testing.T) {
	t.Helper()
	stored, err := rig.store.List(t.Context(), failure.Query{})
	if err != nil {
		t.Fatal(err)
	}
	if len(stored) != 0 || len(rig.sink.all()) != 0 {
		t.Fatalf("stored %d records and delivered %d, want none", len(stored), len(rig.sink.all()))
	}
}

func (rig *failureLogRig) requireCorrelation(t *testing.T, id string, want failure.Correlation) {
	t.Helper()
	local, err := rig.store.Local(t.Context(), id)
	if err != nil {
		t.Fatal(err)
	}
	if local.Correlation != want {
		t.Fatalf("correlation = %+v, want %+v", local.Correlation, want)
	}
}

func requireUnavailableStep(t *testing.T, record failure.Record, reason string) {
	t.Helper()
	step := record.FailingStep
	if step == nil || step.Status != failure.StatusUnavailable || step.Reason != reason {
		t.Fatalf("failing step = %+v, want unavailable with %q", step, reason)
	}
}

type faultTool struct {
	name   string
	result tool.Result
	err    error
	panics string
}

func (f faultTool) Spec() tool.Spec {
	return tool.Spec{Name: f.name, Description: "stub", Schema: jsontext.Value(`{"type":"object"}`)}
}

func (faultTool) Class() tool.Class { return tool.ClassRead }

func (f faultTool) Run(context.Context, tool.Call, tool.Env) (tool.Result, error) {
	if f.panics != "" {
		panic(f.panics)
	}
	return f.result, f.err
}

func runOneCall(t *testing.T, h *harness, name, args string) (*Thread, event.ToolCompleted) {
	t.Helper()
	h.model.push(calls(call{id: "c1", name: name, args: args}), text("done"))
	thread := h.start()
	h.submit(thread, "run the tool")
	h.waitIdle(thread)
	completed, ok := completedOutputs(h.events(thread.ID()))["c1"]
	if !ok {
		t.Fatalf("no tool.completed for c1: %v", h.types(thread.ID()))
	}
	return thread, completed
}

func TestInternalToolFailureCommitsOneMaskedRecord(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	broken := faultTool{name: "broken", result: tool.Result{Failure: tool.FailureInternal}, err: errors.New("helper lost its socket holding " + sampleSecret)}
	h := newHarness(t, withFailureLog(rig), withExtraTool(broken))
	thread, completed := runOneCall(t, h, "broken", secretNote)
	if completed.OK || completed.Failure != "internal" {
		t.Fatalf("tool.completed = ok %v failure %q, want a failed call of kind internal", completed.OK, completed.Failure)
	}
	record := rig.onlyRecord(t)
	requireMaskedToolRecord(t, record)
	rig.requireCorrelation(t, record.ID, failure.Correlation{Session: thread.ID(), Turn: lastTurn(h.events(thread.ID())).TurnID, CallID: "c1"})
	if lastTurn(h.events(thread.ID())).Status != statusCompleted {
		t.Fatalf("turn did not continue after the tool failure: %v", h.types(thread.ID()))
	}
}

func requireMaskedToolRecord(t *testing.T, record failure.Record) {
	t.Helper()
	if record.Class != failure.ClassTool || record.Tool != "broken" || record.Product != "dalgona" {
		t.Fatalf("record = %+v, want a dalgona tool record for broken", record)
	}
	if !record.Reportable || record.ContentFree || !strings.Contains(record.Error, "helper lost its socket") {
		t.Fatalf("record = %+v, want a reportable masked record that keeps the error", record)
	}
	if record.FailingStep == nil {
		t.Fatal("record has no failing step")
	}
}

func TestOrdinaryToolErrorCommitsNoRecord(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	missing := faultTool{name: "lookup", err: errors.New("lookup: notes.txt does not exist")}
	h := newHarness(t, withFailureLog(rig), withExtraTool(missing))
	_, completed := runOneCall(t, h, "lookup", secretNote)
	if completed.OK || completed.Failure != "" {
		t.Fatalf("tool.completed = ok %v failure %q, want an ordinary failed result", completed.OK, completed.Failure)
	}
	rig.requireNone(t)
}

func TestToolPanicIsContainedAndCommitsOnePanicRecord(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	h := newHarness(t, withFailureLog(rig), withExtraTool(faultTool{name: "fragile", panics: "index out of range near " + sampleSecret}))
	thread, completed := runOneCall(t, h, "fragile", secretNote)
	if completed.OK || completed.Failure != "internal" || strings.Contains(completed.Output, sampleSecret) {
		t.Fatalf("tool.completed = %+v, want a failed internal call without the secret", completed)
	}
	record := rig.onlyRecord(t)
	if record.Class != failure.ClassPanic || record.Tool != "fragile" || !strings.Contains(record.Error, "index out of range") {
		t.Fatalf("record = %+v, want a panic record for fragile", record)
	}
	if !stackNames(record.Stack, "faultTool.Run") {
		t.Fatalf("stack = %+v, want the panicking Run frame", record.Stack)
	}
	requireUnavailableStep(t, record, failure.ReasonPanicNoCall)
	rig.requireCorrelation(t, record.ID, failure.Correlation{Session: thread.ID(), Turn: lastTurn(h.events(thread.ID())).TurnID, CallID: "c1"})
}

func TestToolPanicIsContainedWithoutAFailureStore(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withExtraTool(faultTool{name: "fragile", panics: "index out of range"}))
	thread, completed := runOneCall(t, h, "fragile", `{}`)
	if completed.OK || completed.Failure != "internal" || completed.Output != "failure: recovered panic" {
		t.Fatalf("tool.completed = %+v, want a failed internal call", completed)
	}
	if ended := lastTurn(h.events(thread.ID())); ended.Status != statusCompleted {
		t.Fatalf("turn = %+v, want it to continue past the contained panic", ended)
	}
}

func TestProviderStreamPanicFailsTheTurnAndTheHostKeepsServing(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	h := newHarness(t, withFailureLog(rig))
	var exploded atomic.Bool
	h.model.route = func([]byte) (response, bool) {
		if exploded.CompareAndSwap(false, true) {
			panic("stream decoder exploded")
		}
		return response{}, false
	}
	first := h.start()
	h.submit(first, "hello")
	h.waitIdle(first)
	ended := lastTurn(h.events(first.ID()))
	if ended.Status != statusFailed || ended.Error != "model: provider panicked" {
		t.Fatalf("turn = %+v, want failed with the provider panic", ended)
	}
	record := rig.onlyRecord(t)
	if record.Class != failure.ClassPanic || !strings.Contains(record.Error, "stream decoder exploded") || len(record.Stack) == 0 {
		t.Fatalf("record = %+v, want the panic record with its stack", record)
	}
	rig.requireCorrelation(t, record.ID, failure.Correlation{Session: first.ID(), Turn: ended.TurnID})
	requireServesAnotherThread(t, h)
}

func TestProviderFactoryPanicFailsTheTurnAndTheHostKeepsServing(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	var exploded atomic.Bool
	h := newHarness(t, withFailureLog(rig), func(h *harness) {
		h.cfg.Providers = func(onRetry RetryFunc) (*model.Registry, error) {
			if onRetry != nil && exploded.CompareAndSwap(false, true) {
				panic("provider registry broke")
			}
			return h.providers(onRetry)
		}
	})
	first := h.start()
	h.submit(first, "hello")
	h.waitIdle(first)
	ended := lastTurn(h.events(first.ID()))
	if ended.Status != statusFailed || ended.Error != "failure: recovered panic" {
		t.Fatalf("turn = %+v, want failed with the recovered panic", ended)
	}
	record := rig.onlyRecord(t)
	if record.Class != failure.ClassPanic || !strings.Contains(record.Error, "provider registry broke") {
		t.Fatalf("record = %+v, want the panic record", record)
	}
	requireServesAnotherThread(t, h)
}

func TestActorPanicClosesOnlyItsThread(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	h := newHarness(t, withFailureLog(rig))
	broken := h.start()
	if err := broken.do(t.Context(), func(*actor) { panic("actor state corrupted") }); err != nil {
		t.Fatal(err)
	}
	if _, err := broken.Status(); !errors.Is(err, ErrClosed) {
		t.Fatalf("status after the actor panic = %v, want the thread closed", err)
	}
	record := rig.onlyRecord(t)
	if record.Class != failure.ClassPanic || !strings.Contains(record.Error, "actor state corrupted") {
		t.Fatalf("record = %+v, want the panic record", record)
	}
	rig.requireCorrelation(t, record.ID, failure.Correlation{Session: broken.ID()})
	requireServesAnotherThread(t, h)
}

func requireServesAnotherThread(t *testing.T, h *harness) {
	t.Helper()
	other := h.start()
	h.submit(other, "hello again")
	h.waitIdle(other)
	if ended := lastTurn(h.events(other.ID())); ended.Status != statusCompleted {
		t.Fatalf("another thread ended %+v, want the host to keep serving", ended)
	}
}

func stackNames(frames []failure.Frame, function string) bool {
	for _, frame := range frames {
		if strings.HasSuffix(frame.Function, function) {
			return true
		}
	}
	return false
}

func TestTerminalModelFailureCommitsOneModelRecord(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	h := newHarness(t, withFailureLog(rig))
	h.model.push(failing(400, `{"error":{"message":"request rejected for key `+sampleSecret+`"}}`))
	thread := h.start()
	h.submit(thread, "hello")
	h.waitIdle(thread)
	ended := lastTurn(h.events(thread.ID()))
	if ended.Status != statusFailed {
		t.Fatalf("turn = %+v, want failed", ended)
	}
	record := rig.onlyRecord(t)
	if record.Class != failure.ClassModel || record.Tool != "" || record.Error == "" {
		t.Fatalf("record = %+v, want a model record with the provider error", record)
	}
	requireUnavailableStep(t, record, failure.ReasonProvider)
	rig.requireCorrelation(t, record.ID, failure.Correlation{Session: thread.ID(), Turn: ended.TurnID})
}

func TestInterruptedTurnCommitsNoRecord(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	h := newHarness(t, withFailureLog(rig))
	release := h.holdModel()
	thread := h.start()
	h.submit(thread, "hello")
	h.waitRequests(1)
	thread.Interrupt()
	release.Store(true)
	h.waitIdle(thread)
	if ended := lastTurn(h.events(thread.ID())); ended.Status != statusInterrupted {
		t.Fatalf("turn = %+v, want interrupted", ended)
	}
	rig.requireNone(t)
}

func TestJournalWriteFailureCommitsOneJournalRecord(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	h := newHarness(t, withFailureLog(rig))
	thread := h.start()
	if err := h.journal.Close(); err != nil {
		t.Fatal(err)
	}
	for range 2 {
		if err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "hello"}); err == nil {
			t.Fatal("submit after the journal closed succeeded")
		}
	}
	record := rig.onlyRecord(t)
	if record.Class != failure.ClassJournal || record.Tool != "" {
		t.Fatalf("record = %+v, want one journal record", record)
	}
	requireUnavailableStep(t, record, failure.ReasonEnv)
	rig.requireCorrelation(t, record.ID, failure.Correlation{Session: thread.ID()})
}

func TestRecoveryHaltCommitsOneRuntimeRecord(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	h := newHarness(t, withFailureLog(rig))
	h.seed(prefix(openTurn("loop"), recoveryRecords(3))...)
	host := h.newHost()
	if err := host.Recover(t.Context()); err != nil {
		t.Fatal(err)
	}
	requireGaveUp(t, h.events(crashSession))
	record := rig.onlyRecord(t)
	if record.Class != failure.ClassRuntime || record.Tool != "" {
		t.Fatalf("record = %+v, want one runtime record", record)
	}
	requireUnavailableStep(t, record, failure.ReasonSandbox)
	rig.requireCorrelation(t, record.ID, failure.Correlation{Session: crashSession, Turn: "t"})
}

func TestRefusedFailureRecordIsNeverDelivered(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	broken := faultTool{name: "broken", result: tool.Result{Failure: tool.FailureInternal}, err: errors.New("helper lost its socket")}
	h := newHarness(t, withFailureLog(rig), withExtraTool(broken))
	if err := os.RemoveAll(rig.dir); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(rig.dir, []byte("not a directory"), 0o600); err != nil {
		t.Fatal(err)
	}
	_, completed := runOneCall(t, h, "broken", secretNote)
	if completed.Failure != "internal" {
		t.Fatalf("tool.completed failure = %q, want internal", completed.Failure)
	}
	if delivered := rig.sink.all(); len(delivered) != 0 {
		t.Fatalf("delivered %d refused records", len(delivered))
	}
	if !strings.Contains(h.logs.String(), "failure record was not written") {
		t.Fatalf("log has no refusal line: %s", h.logs.String())
	}
}

func TestMaskingFailureDeliversOnlyContentFreeRecord(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	broken := faultTool{name: "broken", result: tool.Result{Failure: tool.FailureInternal}, err: errors.New("helper lost its socket holding " + sampleSecret)}
	h := newHarness(t, withExtraTool(broken), func(h *harness) {
		store, err := failure.Open(h.t.Context(), failure.Options{Home: h.home, Product: "dalgona", Masker: failingMasker{}})
		if err != nil {
			h.t.Fatal(err)
		}
		rig.store, rig.sink, rig.dir = store, &recordSink{}, filepath.Join(h.home, "failures")
		h.cfg.Failures, h.cfg.FailureSink = store, rig.sink.take
	})
	runOneCall(t, h, "broken", secretNote)
	record := rig.onlyRecord(t)
	if !record.ContentFree || record.Error != "failure details withheld: masking failed" || record.Args != "" || len(record.Stack) != 0 {
		t.Fatalf("record = %+v, want the content-free fallback", record)
	}
	requireUnavailableStep(t, record, failure.ReasonMaskingFailed)
}

type failingMasker struct{}

func (failingMasker) MaskWithError(string) (string, error) {
	return "", errors.New("secret masking failed: pattern state was not persisted")
}

func TestAdvisorFailureIsCapturedLocallyAsSideOrigin(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	h := newHarness(t, withFailureLog(rig))
	parent := h.start()
	advisor, err := h.host.Start(t.Context(), ThreadSpec{Parent: parent.ID(), Advisor: "critic", Overrides: Overrides{Tools: []tool.Tool{}, System: "critic"}})
	if err != nil {
		t.Fatal(err)
	}
	h.model.push(failing(400, `{"error":{"message":"request rejected"}}`))
	h.submit(advisor, "review")
	h.waitIdle(advisor)
	record := rig.onlyRecord(t)
	if record.Class != failure.ClassModel || record.Reportable {
		t.Fatalf("record = %+v, want a local model record that is not reportable", record)
	}
}

type unstartableRunner struct{ runner.Runner }

func (unstartableRunner) Start(context.Context, runner.ProcessSpec) error {
	return errors.New("fork/exec /bin/sh: resource temporarily unavailable")
}

func TestExecStartFailureCommitsOneToolRecord(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	h := newHarness(t, withApproval(config.ApprovalYolo), withFailureLog(rig), func(h *harness) {
		h.cfg.Runner = func(ctx context.Context, id, name string) (runner.Runner, error) {
			local, err := h.runner(ctx, id, name)
			return unstartableRunner{local}, err
		}
	})
	thread, completed := runOneCall(t, h, "exec", `{"command":"echo hi"}`)
	if completed.OK || completed.Failure != "internal" {
		t.Fatalf("tool.completed = ok %v failure %q, want a failed call of kind internal", completed.OK, completed.Failure)
	}
	record := rig.onlyRecord(t)
	if record.Class != failure.ClassTool || record.Tool != "exec" || !strings.Contains(record.Error, "resource temporarily unavailable") {
		t.Fatalf("record = %+v, want an exec tool record with the start error", record)
	}
	rig.requireCorrelation(t, record.ID, failure.Correlation{Session: thread.ID(), Turn: lastTurn(h.events(thread.ID())).TurnID, CallID: "c1"})
}

func TestOrdinaryCommandExitCommitsNoRecord(t *testing.T) {
	t.Parallel()
	rig := &failureLogRig{}
	h := newHarness(t, withApproval(config.ApprovalYolo), withFailureLog(rig))
	_, completed := runOneCall(t, h, "exec", `{"command":"exit 3"}`)
	if completed.ExitCode == nil || *completed.ExitCode != 3 || completed.Failure != "" {
		t.Fatalf("tool.completed = %+v, want an ordinary exit 3", completed)
	}
	rig.requireNone(t)
}
