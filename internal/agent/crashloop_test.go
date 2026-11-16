package agent

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"slices"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

type bootEntry struct {
	name string
	open func(context.Context, *Host) error
}

func bootEntries() []bootEntry {
	return []bootEntry{
		{name: "Recover", open: func(ctx context.Context, host *Host) error { return host.Recover(ctx) }},
		{name: "Resume", open: func(ctx context.Context, host *Host) error {
			_, err := host.Resume(ctx, crashSession)
			return err
		}},
		{name: "child open", open: func(ctx context.Context, host *Host) error {
			_, err := host.openChild(ctx, ThreadSpec{}, crashSession)
			return err
		}},
	}
}

type loopProbeTool struct{ runs *atomic.Int32 }

func (loopProbeTool) Spec() tool.Spec {
	return tool.Spec{Name: "probe", Description: "stub", Schema: jsontext.Value(`{"type":"object"}`)}
}

func (loopProbeTool) Class() tool.Class { return tool.ClassRead }

func (p loopProbeTool) Run(context.Context, tool.Call, tool.Env) (tool.Result, error) {
	p.runs.Add(1)
	return tool.Result{Output: []byte("probed"), OK: true}, nil
}

func withProbe(runs *atomic.Int32) func(*harness) {
	return func(h *harness) {
		h.cfg.Tools = func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{loopProbeTool{runs: runs}}, nil
		}
	}
}

func recoveryRecords(attempts int) []event.Event {
	out := make([]event.Event, attempts)
	for i := range out {
		out[i] = event.TurnRecovery{TurnID: "t", Attempt: i + 1}
	}
	return out
}

func recoveryAttempts(events []event.Seqd, turnID string) []int {
	var out []int
	for _, item := range events {
		if value, ok := item.Event.(event.TurnRecovery); ok && value.TurnID == turnID {
			out = append(out, value.Attempt)
		}
	}
	return out
}

func lastTurnStarted(events []event.Seqd) string {
	for _, item := range slices.Backward(events) {
		if value, ok := item.Event.(event.TurnStarted); ok {
			return value.TurnID
		}
	}
	return ""
}

func requireGaveUp(t *testing.T, events []event.Seqd) {
	t.Helper()
	ended := lastTurn(events)
	if ended.TurnID != "t" || ended.Status != "failed" || ended.Error != "recovery gave up after 3 attempts" {
		t.Fatalf("turn.completed = %+v, want failed with the give-up error", ended)
	}
}

func (h *harness) holdModel() *atomic.Bool {
	var release atomic.Bool
	hold := make(chan struct{})
	h.model.route = func([]byte) (response, bool) {
		if release.Load() {
			return response{}, false
		}
		return response{body: sseText("never"), gate: hold}, true
	}
	return &release
}

func (h *harness) crashAfterBoot(entry bootEntry, boot int) {
	h.t.Helper()
	host := h.newHost()
	if err := entry.open(h.t.Context(), host); err != nil {
		h.t.Fatalf("boot %d: %v", boot, err)
	}
	want := make([]int, boot)
	for i := range want {
		want[i] = i + 1
	}
	if got := recoveryAttempts(h.events(crashSession), "t"); !slices.Equal(got, want) {
		h.t.Fatalf("boot %d: open returned with turn.recovery attempts %v, want %v", boot, got, want)
	}
	h.waitRequests(boot)
	if err := host.Close(); err != nil {
		h.t.Fatalf("boot %d: close: %v", boot, err)
	}
}

func (h *harness) requireAbandoned() {
	h.t.Helper()
	events := h.events(crashSession)
	if got := recoveryAttempts(events, "t"); !slices.Equal(got, []int{1, 2, 3}) {
		h.t.Fatalf("boot 4 journaled turn.recovery attempts %v, want [1 2 3]", got)
	}
	if _, closed := events[len(events)-1].Event.(event.TurnCompleted); !closed {
		h.t.Fatalf("boot 4 left the turn open: %v", h.types(crashSession))
	}
	requireGaveUp(h.t, events)
	if h.model.count() != 3 {
		h.t.Fatalf("model requests = %d, want 3: boot 4 must not call the model", h.model.count())
	}
	if !strings.Contains(h.logs.String(), "recovery halted after 3 attempts") {
		h.t.Fatalf("log has no halt line: %s", h.logs.String())
	}
}

func (h *harness) reopenedIdle(host *Host) *Thread {
	h.t.Helper()
	thread, err := host.Resume(h.t.Context(), crashSession)
	if err != nil {
		h.t.Fatal(err)
	}
	status, err := thread.Status()
	if err != nil {
		h.t.Fatal(err)
	}
	if status.Busy || status.State != event.StatusIdle || status.Turn != "" {
		h.t.Fatalf("thread after boot 4 = %+v, want idle", status)
	}
	return thread
}

func (h *harness) crashNewTurn(host *Host, thread *Thread) {
	h.t.Helper()
	h.submit(thread, "again")
	h.waitRequests(4)
	if err := host.Close(); err != nil {
		h.t.Fatal(err)
	}
}

func (h *harness) bootNewTurn(entry bootEntry) (*Host, string) {
	h.t.Helper()
	revived := h.newHost()
	if err := entry.open(h.t.Context(), revived); err != nil {
		h.t.Fatal(err)
	}
	events := h.events(crashSession)
	next := lastTurnStarted(events)
	if next == "" || next == "t" {
		h.t.Fatalf("new turn id = %q", next)
	}
	if got := recoveryAttempts(events, next); !slices.Equal(got, []int{1}) {
		h.t.Fatalf("new turn turn.recovery attempts %v, want [1]", got)
	}
	if got := recoveryAttempts(events, "t"); !slices.Equal(got, []int{1, 2, 3}) {
		h.t.Fatalf("old turn turn.recovery attempts changed to %v", got)
	}
	return revived, next
}

func (h *harness) requireTurnCompletes(host *Host, turnID string) {
	h.t.Helper()
	thread, err := host.Resume(h.t.Context(), crashSession)
	if err != nil {
		h.t.Fatal(err)
	}
	h.waitIdle(thread)
	if ended := lastTurn(h.events(crashSession)); ended.TurnID != turnID || ended.Status != statusCompleted {
		h.t.Fatalf("new turn after the halt ended as %+v", ended)
	}
}

func TestCrashLoopBreakerStopsAtThreeAttempts(t *testing.T) {
	t.Parallel()
	for _, entry := range bootEntries() {
		t.Run(entry.name, func(t *testing.T) {
			t.Parallel()
			h := newHarness(t)
			release := h.holdModel()
			h.seed(openTurn("loop")...)
			for boot := 1; boot <= 3; boot++ {
				h.crashAfterBoot(entry, boot)
			}
			host := h.newHost()
			if err := entry.open(t.Context(), host); err != nil {
				t.Fatal(err)
			}
			h.requireAbandoned()
			h.crashNewTurn(host, h.reopenedIdle(host))
			release.Store(true)
			revived, next := h.bootNewTurn(entry)
			h.requireTurnCompletes(revived, next)
		})
	}
}

func pendingProbeCall() []event.Event {
	return prefix(
		openTurn("go"),
		[]event.Event{assistantCall("c1", "probe", `{}`)},
		[]event.Event{startedCall("c1", "probe", "read", `{}`)},
	)
}

func TestCrashLoopBreakerCompletesPendingCallsWithoutReplay(t *testing.T) {
	t.Parallel()
	var runs atomic.Int32
	h := newHarness(t, withProbe(&runs))
	h.seed(prefix(pendingProbeCall(), recoveryRecords(3))...)
	host := h.newHost()
	if _, err := host.Resume(t.Context(), crashSession); err != nil {
		t.Fatal(err)
	}
	events := h.events(crashSession)
	got := completedOutputs(events)["c1"]
	if got.OK || got.Recovered != "interrupted" || !strings.HasPrefix(got.Output, "interrupted:") {
		t.Fatalf("call completion at boot 4 = %+v", got)
	}
	if runs.Load() != 0 || h.model.count() != 0 {
		t.Fatalf("boot 4 ran the tool %d times and the model %d times, want 0 and 0", runs.Load(), h.model.count())
	}
	requireGaveUp(t, events)
}

func TestCrashLoopBreakerReplaysPendingCallsBelowTheLimit(t *testing.T) {
	t.Parallel()
	var runs atomic.Int32
	h := newHarness(t, withProbe(&runs))
	h.seed(prefix(pendingProbeCall(), recoveryRecords(2))...)
	events := h.recoverSession()
	if done := completedOutputs(events)["c1"]; !done.OK || done.Recovered != "replayed" || runs.Load() != 1 {
		t.Fatalf("boot 3 completion = %+v with %d runs, want replayed with 1 run", done, runs.Load())
	}
	if got := recoveryAttempts(events, "t"); !slices.Equal(got, []int{1, 2, 3}) {
		t.Fatalf("boot 3 turn.recovery attempts %v, want [1 2 3]", got)
	}
}

func (h *harness) requireFailedOpen(host *Host) {
	h.t.Helper()
	before := len(h.events(crashSession))
	cancelled, cancel := context.WithCancel(h.t.Context())
	cancel()
	_, err := host.Resume(cancelled, crashSession)
	if !errors.Is(err, context.Canceled) {
		h.t.Fatalf("open with a failing append returned %v, want context.Canceled", err)
	}
	if _, open := host.loadedThread(crashSession); open {
		h.t.Fatal("a failed open left the thread registered")
	}
	if after := len(h.events(crashSession)); after != before || h.model.count() != 0 {
		h.t.Fatalf("failed open journaled %d records and made %d model requests, want 0 and 0", after-before, h.model.count())
	}
}

func (h *harness) requireRetried(prior int) {
	h.t.Helper()
	events := h.events(crashSession)
	ended := lastTurn(events)
	attempts := recoveryAttempts(events, "t")
	if prior == 0 && (ended.Status != statusCompleted || !slices.Equal(attempts, []int{1})) {
		h.t.Fatalf("retry after a failed attempt record: turn %+v, attempts %v", ended, attempts)
	}
	if prior == 3 && (ended.Status != "failed" || h.model.count() != 0) {
		h.t.Fatalf("retry after a failed give-up record: turn %+v, model requests %d", ended, h.model.count())
	}
}

func TestCrashLoopBreakerFailsOpenWhenAnAppendFails(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name  string
		prior int
	}{
		{name: "attempt record", prior: 0},
		{name: "give-up record", prior: 3},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			h := newHarness(t)
			h.seed(prefix(openTurn("go"), recoveryRecords(test.prior))...)
			host := h.newHost()
			h.requireFailedOpen(host)
			thread, err := host.Resume(t.Context(), crashSession)
			if err != nil {
				t.Fatalf("open after the failed append: %v", err)
			}
			h.waitIdle(thread)
			h.requireRetried(test.prior)
		})
	}
}
