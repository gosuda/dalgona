package agent

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"slices"
	"strings"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

type evidenceLog struct {
	mu       sync.Mutex
	evidence map[string]snapshot.Evidence
	refs     map[string]string
	verdicts map[string]error
}

func newEvidenceLog() *evidenceLog {
	return &evidenceLog{
		evidence: make(map[string]snapshot.Evidence),
		refs:     make(map[string]string),
		verdicts: make(map[string]error),
	}
}

func (l *evidenceLog) saw(id string, evidence snapshot.Evidence) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.evidence[id] = evidence
}

func (l *evidenceLog) minted(id, ref string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.refs[id] = ref
}

func (l *evidenceLog) judged(id string, verdict error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.verdicts[id] = verdict
}

func (l *evidenceLog) evidenceOf(id string) snapshot.Evidence {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.evidence[id]
}

func (l *evidenceLog) refOf(id string) string {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.refs[id]
}

func (l *evidenceLog) verdictOf(id string) error {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.verdicts[id]
}

type deliverFake struct {
	name   string
	class  tool.Class
	log    *evidenceLog
	filler string
}

func (d deliverFake) Spec() tool.Spec {
	return tool.Spec{Name: d.name, Description: "mints a snapshot", Schema: jsontext.Value(`{"type":"object"}`)}
}

func (d deliverFake) Class() tool.Class { return d.class }

func (d deliverFake) Run(_ context.Context, call tool.Call, env tool.Env) (tool.Result, error) {
	d.log.saw(call.ID, env.Evidence)
	entry, ok := env.Snapshots.Capture(d.name+".txt", []byte("one\ntwo\n"), []snapshot.Range{{Start: 1, End: 2}})
	if !ok {
		return tool.Result{}, errors.New("capture refused")
	}
	d.log.minted(call.ID, entry.Ref)
	output := d.filler + "1:one\n2:two\n[" + snapshot.HeaderPath(entry.Path) + "@" + entry.Ref + "]\n"
	return tool.Result{Output: []byte(output), OK: true}, nil
}

type probeFake struct {
	name string
	of   string
	log  *evidenceLog
}

func (p probeFake) Spec() tool.Spec {
	return tool.Spec{Name: p.name, Description: "checks a delivered reference", Schema: jsontext.Value(`{"type":"object"}`)}
}

func (p probeFake) Class() tool.Class { return tool.ClassRead }

func (p probeFake) Run(_ context.Context, call tool.Call, env tool.Env) (tool.Result, error) {
	p.log.saw(call.ID, env.Evidence)
	p.log.judged(call.ID, env.Snapshots.Eligible(p.log.refOf(p.of), env.Evidence))
	return tool.Result{Output: []byte("probed"), OK: true}, nil
}

func evidenceHarness(t *testing.T, extra ...tool.Tool) *harness {
	t.Helper()
	return newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) { h.extraTools = extra })
}

func runEvidence(t *testing.T, h *harness, rounds ...response) (*Thread, []event.Seqd) {
	t.Helper()
	h.model.push(append(rounds, text("done"))...)
	thread := h.start()
	h.submit(thread, "run the task")
	h.waitIdle(thread)
	return thread, h.events(thread.ID())
}

func completionSeq(t *testing.T, events []event.Seqd, callID string) uint64 {
	t.Helper()
	for _, item := range events {
		if value, ok := item.Event.(event.ToolCompleted); ok && value.CallID == callID {
			return item.Seq
		}
	}
	t.Fatalf("agent: journal has no tool.completed for %s", callID)
	return 0
}

func startedSeq(t *testing.T, events []event.Seqd, callID string) uint64 {
	t.Helper()
	for _, item := range events {
		if value, ok := item.Event.(event.ToolStarted); ok && value.CallID == callID {
			return item.Seq
		}
	}
	t.Fatalf("agent: journal has no tool.started for %s", callID)
	return 0
}

func carrierOf(t *testing.T, events []event.Seqd, callID string) (uint64, event.AssistantMessage) {
	t.Helper()
	for _, item := range events {
		message, ok := item.Event.(event.AssistantMessage)
		if ok && slices.ContainsFunc(message.ToolCalls, func(c event.ToolCall) bool { return c.ID == callID }) {
			return item.Seq, message
		}
	}
	t.Fatalf("agent: journal has no assistant.message carrying %s", callID)
	return 0, event.AssistantMessage{}
}

func modelEvidence(cutoff uint64) snapshot.Evidence {
	return snapshot.Evidence{Consumer: snapshot.ConsumerModel, Cutoff: cutoff}
}

func requireUnknownReference(t *testing.T, err error, what string) {
	t.Helper()
	if !errors.Is(err, snapshot.ErrUnknownReference) {
		t.Fatalf("agent: %s = %v, want %v", what, err, snapshot.ErrUnknownReference)
	}
}

func TestEvidenceCutoffFollowsJournaledRequest(t *testing.T) {
	t.Parallel()
	log := newEvidenceLog()
	h := evidenceHarness(t,
		deliverFake{name: "deliver", class: tool.ClassRead, log: log},
		probeFake{name: "probe", of: "d1", log: log},
	)
	thread, events := runEvidence(t, h,
		calls(call{id: "d1", name: "deliver", args: `{}`}),
		calls(call{id: "p1", name: "probe", args: `{}`}),
	)
	delivered := completionSeq(t, events, "d1")
	messageSeq, message := carrierOf(t, events, "p1")
	if message.ContextSeq < delivered || message.ContextSeq >= messageSeq {
		t.Fatalf("agent: request context_seq %d must include the delivery at %d and precede its message at %d", message.ContextSeq, delivered, messageSeq)
	}
	if got := log.evidenceOf("p1"); got != modelEvidence(message.ContextSeq) {
		t.Fatalf("agent: evidence of p1 = %+v, want the journaled request cutoff %d", got, message.ContextSeq)
	}
	if err := log.verdictOf("p1"); err != nil {
		t.Fatalf("agent: reference delivered before the request is not eligible: %v", err)
	}
	ref := log.refOf("d1")
	if err := thread.snapshots.Eligible(ref, modelEvidence(delivered)); err != nil {
		t.Fatalf("agent: reference at its delivery sequence %d = %v, want eligible", delivered, err)
	}
	requireUnknownReference(t, thread.snapshots.Eligible(ref, modelEvidence(delivered-1)), "reference one sequence before its delivery")
	firstSeq, first := carrierOf(t, events, "d1")
	if got := log.evidenceOf("d1"); got != modelEvidence(first.ContextSeq) || first.ContextSeq >= delivered || firstSeq >= delivered {
		t.Fatalf("agent: evidence of d1 = %+v, want the cutoff %d of its own request", got, first.ContextSeq)
	}
}

func TestEvidenceCutoffNeedsTheDeliveredHeader(t *testing.T) {
	t.Parallel()
	log := newEvidenceLog()
	h := evidenceHarness(t,
		deliverFake{name: "deliver", class: tool.ClassRead, log: log},
		deliverFake{name: "truncated", class: tool.ClassRead, log: log, filler: strings.Repeat("filler row of text\n", 4000)},
	)
	thread, events := runEvidence(t, h,
		calls(call{id: "d1", name: "deliver", args: `{}`}, call{id: "t1", name: "truncated", args: `{}`}),
	)
	outputs := completedOutputs(events)
	whole, cut := log.refOf("d1"), log.refOf("t1")
	if !strings.Contains(outputs["d1"].Output, "@"+whole+"]") {
		t.Fatalf("agent: control output lost its header: %q", outputs["d1"].Output)
	}
	if outputs["t1"].Spill == nil || strings.Contains(outputs["t1"].Output, cut) {
		t.Fatalf("agent: precondition failed: the projection must be spilled without its header (spill %v)", outputs["t1"].Spill)
	}
	horizon := modelEvidence(uint64(len(events)))
	if err := thread.snapshots.Eligible(whole, horizon); err != nil {
		t.Fatalf("agent: delivered header makes the reference ineligible: %v", err)
	}
	requireUnknownReference(t, thread.snapshots.Eligible(cut, horizon), "reference whose header was cut from the projection")
}

func TestParallelCallsShareCutoff(t *testing.T) {
	t.Parallel()
	log := newEvidenceLog()
	h := evidenceHarness(t,
		deliverFake{name: "deliver", class: tool.ClassRead, log: log},
		probeFake{name: "probe", of: "d1", log: log},
	)
	_, events := runEvidence(t, h,
		calls(call{id: "d1", name: "deliver", args: `{}`}, call{id: "p1", name: "probe", args: `{}`}),
		calls(call{id: "p2", name: "probe", args: `{}`}),
	)
	if completionSeq(t, events, "d1") >= startedSeq(t, events, "p1") {
		t.Fatal("agent: precondition failed: p1 must start after d1 completed")
	}
	firstSeq, first := carrierOf(t, events, "d1")
	secondSeq, _ := carrierOf(t, events, "p1")
	if firstSeq != secondSeq {
		t.Fatalf("agent: d1 and p1 must share one assistant.message, got seq %d and %d", firstSeq, secondSeq)
	}
	want := modelEvidence(first.ContextSeq)
	if log.evidenceOf("d1") != want || log.evidenceOf("p1") != want {
		t.Fatalf("agent: evidence d1 = %+v, p1 = %+v, want both %+v", log.evidenceOf("d1"), log.evidenceOf("p1"), want)
	}
	requireUnknownReference(t, log.verdictOf("p1"), "a preview from a sibling call that completed before it ran")
	_, later := carrierOf(t, events, "p2")
	if log.evidenceOf("p2") != modelEvidence(later.ContextSeq) || later.ContextSeq <= first.ContextSeq {
		t.Fatalf("agent: evidence of p2 = %+v, want the larger cutoff %d of the next request", log.evidenceOf("p2"), later.ContextSeq)
	}
	if err := log.verdictOf("p2"); err != nil {
		t.Fatalf("agent: a later request cannot use the delivery of the earlier round: %v", err)
	}
}

func TestParallelCallsShareCutoffAgentBatch(t *testing.T) {
	t.Parallel()
	log := newEvidenceLog()
	h := evidenceHarness(t,
		deliverFake{name: "fana", class: tool.ClassAgent, log: log},
		deliverFake{name: "fanb", class: tool.ClassAgent, log: log},
	)
	thread, events := runEvidence(t, h,
		calls(call{id: "a1", name: "fana", args: `{}`}, call{id: "b1", name: "fanb", args: `{}`}),
	)
	firstSeq, message := carrierOf(t, events, "a1")
	secondSeq, _ := carrierOf(t, events, "b1")
	if firstSeq != secondSeq {
		t.Fatalf("agent: a1 and b1 must share one assistant.message, got seq %d and %d", firstSeq, secondSeq)
	}
	want := modelEvidence(message.ContextSeq)
	if log.evidenceOf("a1") != want || log.evidenceOf("b1") != want {
		t.Fatalf("agent: evidence a1 = %+v, b1 = %+v, want both %+v", log.evidenceOf("a1"), log.evidenceOf("b1"), want)
	}
	for _, id := range []string{"a1", "b1"} {
		delivered := completionSeq(t, events, id)
		if err := thread.snapshots.Eligible(log.refOf(id), modelEvidence(delivered)); err != nil {
			t.Fatalf("agent: %s at its own delivery sequence %d = %v, want eligible", id, delivered, err)
		}
		requireUnknownReference(t, thread.snapshots.Eligible(log.refOf(id), modelEvidence(delivered-1)), id+" one sequence before its delivery")
	}
	if completionSeq(t, events, "b1") != completionSeq(t, events, "a1")+1 {
		t.Fatalf("agent: batch completions must be adjacent: a1 %d, b1 %d", completionSeq(t, events, "a1"), completionSeq(t, events, "b1"))
	}
}

func TestEvidenceCutoffInBand(t *testing.T) {
	t.Parallel()
	log := newEvidenceLog()
	h := newHarness(t, func(h *harness) {
		h.cfg.Tools = func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{
				deliverFake{name: "deliver", class: tool.ClassRead, log: log},
				probeFake{name: "probe", of: "d1", log: log},
			}, nil
		}
	})
	thread := h.start()
	startInBandTurn(t, thread)
	tools, err := thread.toolsFor(t.Context(), "openai/gpt-test")
	if err != nil {
		t.Fatalf("agent: tools: %v", err)
	}
	w := &worker{t: thread, turnID: "turn-test", settings: Settings{Approval: config.ApprovalYolo}, tools: tools, inBand: newInBandCalls()}
	for _, item := range []model.ToolCall{
		{ID: "d1", Name: "deliver", Args: jsontext.Value(`{}`)},
		{ID: "p1", Name: "probe", Args: jsontext.Value(`{}`)},
	} {
		if _, err := w.inBandEvents(t.Context(), item); err != nil {
			t.Fatalf("agent: in-band %s: %v", item.ID, err)
		}
	}
	events := h.events(thread.ID())
	delivered := completionSeq(t, events, "d1")
	_, cover := carrierOf(t, events, "p1")
	if cover.ContextSeq < delivered {
		t.Fatalf("agent: cover context_seq %d must include the delivery at %d", cover.ContextSeq, delivered)
	}
	if got := log.evidenceOf("p1"); got != modelEvidence(cover.ContextSeq) {
		t.Fatalf("agent: in-band evidence of p1 = %+v, want the cover cutoff %d", got, cover.ContextSeq)
	}
	if err := log.verdictOf("p1"); err != nil {
		t.Fatalf("agent: in-band delivery was not bound: %v", err)
	}
	requireUnknownReference(t, thread.snapshots.Eligible(log.refOf("d1"), modelEvidence(delivered-1)), "in-band reference one sequence before its delivery")
}

func TestEvidenceCutoffOf(t *testing.T) {
	t.Parallel()
	turn := func(seq uint64) event.Seqd { return event.Seqd{Seq: seq, Event: event.TurnStarted{TurnID: "t"}} }
	message := func(seq, contextSeq uint64, ids ...string) event.Seqd {
		carried := make([]event.ToolCall, len(ids))
		for i, id := range ids {
			carried[i] = event.ToolCall{ID: id}
		}
		return event.Seqd{Seq: seq, Event: event.AssistantMessage{ToolCalls: carried, ContextSeq: contextSeq}}
	}
	done := func(seq uint64, id string) event.Seqd {
		return event.Seqd{Seq: seq, Event: event.ToolCompleted{CallID: id}}
	}
	cases := []struct {
		name    string
		records []event.Seqd
		callID  string
		want    uint64
	}{
		{"carried by the request message", []event.Seqd{turn(1), message(2, 1, "c1")}, "c1", 1},
		{"sibling calls read the same message", []event.Seqd{turn(1), message(2, 1, "c1", "c2"), done(3, "c1")}, "c2", 1},
		{"a reused id takes the latest carrier", []event.Seqd{turn(1), message(2, 1, "c1"), done(3, "c1"), message(4, 3, "c1")}, "c1", 3},
		{"a completed id with no new carrier is an in-band call", []event.Seqd{turn(1), message(2, 1, "c1"), done(3, "c1")}, "c1", 3},
		{"the scan stops at the turn start", []event.Seqd{message(1, 9, "c1"), turn(2)}, "c1", 2},
		{"a nested id has no carrier", []event.Seqd{turn(1), message(2, 1, "c1")}, "c1/1", 2},
		{"a message without context_seq gives no cutoff", []event.Seqd{turn(1), message(2, 0, "c1")}, "c1", 0},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			if got := cutoffOf(view{records: test.records}, test.callID); got != test.want {
				t.Fatalf("agent: cutoffOf(%s) = %d, want %d", test.callID, got, test.want)
			}
		})
	}
}

func TestEvidenceCutoffHeaderScan(t *testing.T) {
	t.Parallel()
	store := snapshot.New()
	first := store.Put("f.go", []byte("a\n"), nil)
	second := store.Put("g.go", []byte("b\n"), nil)
	odd := snapshot.HeaderPath(`we]ird@name.go`)
	cases := []struct {
		name   string
		output string
		want   []string
	}{
		{"read header as the last line", "1:a\n[f.go@" + first + "]\n", []string{first}},
		{"quoted path holding a bracket and an at sign", "1:a\n[" + odd + "@" + first + "]\n", []string{first}},
		{"search lists tags and then the reference", "[f.go#AB12]\n[f.go#CD34]\n[f.go@" + first + "]\n", []string{first}},
		{"two headers in order", "[f.go@" + first + "]\n[g.go@" + second + "]", []string{first, second}},
		{"carriage return framing", "[f.go@" + first + "]\r\n", []string{first}},
		{"a content row is not a header", "3:[f.go@" + first + "]\n", nil},
		{"a cut header is not a header", "[f.go@" + first, nil},
		{"a tag header holds no reference", "[f.go#AB12]\n", nil},
		{"text that is not a reference", "[f.go@r123.1]\n", nil},
		{"empty output", "", nil},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			if got := deliveredRefs(test.output); !slices.Equal(got, test.want) {
				t.Fatalf("agent: deliveredRefs = %v, want %v", got, test.want)
			}
		})
	}
}
