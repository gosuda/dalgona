package agent

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

const (
	appliedLightMeta = `{"hashline":{"contract_version":1,"style":"hashline-light","status":"applied","commit_state":"complete","reason":null,"error_class":null,"replayed":false,"recovery":null,"path_states":[]}}`
	unknownRefEdit   = `{"input":"[a.txt@r00000000000000000000000000000001.1]\nPUT 1:\n+new\n"}`
	seededIdentity   = "crashed/3/t/0/model/c1"
	createBody       = "[n.txt#NEW]\nPUT >$:\n+body\n"
)

type inBandOutcome struct {
	completed event.ToolCompleted
	err       error
}

func inBandProfileWorker(t *testing.T, approval config.Approval, configure ...func(*harness)) (*harness, *Thread, *worker) {
	t.Helper()
	h := newProfileHarness(t, "hashline-light", configure...)
	thread := h.start()
	startInBandTurn(t, thread)
	tools, err := thread.toolsFor(t.Context(), "openai/gpt-test")
	if err != nil {
		t.Fatalf("agent: tools: %v", err)
	}
	w := &worker{
		t: thread, turnID: "turn-test", settings: Settings{Approval: approval},
		tools: tools, inBand: newInBandCalls(), turnCtx: t.Context(),
	}
	return h, thread, w
}

func createInBand(input string) model.ToolCall {
	return model.ToolCall{ID: "p1", Name: "patch", Args: jsontext.Value(`{"input":` + string(jsonString(input)) + `}`)}
}

func dispatchInBand(ctx context.Context, w *worker, call model.ToolCall) <-chan inBandOutcome {
	out := make(chan inBandOutcome, 1)
	go func() {
		completed, err := w.inBandEvents(ctx, call)
		out <- inBandOutcome{completed, err}
	}()
	return out
}

func requireReplayOf(t *testing.T, original, replayed event.ToolCompleted) {
	t.Helper()
	if replayed.OK != original.OK || replayed.Output != original.Output {
		t.Fatalf("replay ok=%v output %q, want ok=%v output %q", replayed.OK, replayed.Output, original.OK, original.Output)
	}
	want := decodeHashline(t, original.Meta)
	if want.Replayed {
		t.Fatalf("original meta %s is already replayed", original.Meta)
	}
	want.Replayed = true
	if got := decodeHashline(t, replayed.Meta); !reflect.DeepEqual(got, want) {
		t.Fatalf("replayed meta %s, want the original %s with replayed true", replayed.Meta, original.Meta)
	}
}

func countOf[E event.Event](events []event.Seqd, callID func(E) string, id string) int {
	count := 0
	for _, item := range events {
		if value, ok := item.Event.(E); ok && callID(value) == id {
			count++
		}
	}
	return count
}

func startsOf(events []event.Seqd, id string) int {
	return countOf(events, func(e event.ToolStarted) string { return e.CallID }, id)
}

func approvalsOf(events []event.Seqd, id string) int {
	return countOf(events, func(e event.ApprovalRequested) string { return e.CallID }, id)
}

func awaitApproval(t *testing.T, updates <-chan Update) ApprovalPrompt {
	t.Helper()
	deadline := time.After(30 * time.Second)
	for {
		select {
		case update := <-updates:
			if update.Kind == UpdateApproval && !update.Approval.Resolved {
				return update.Approval
			}
		case <-deadline:
			t.Fatal("no approval prompt arrived")
		}
	}
}

func TestInBandRepeatedInvocationReplaysRecordedResult(t *testing.T) {
	t.Run("REP-01/in-band repeat", func(t *testing.T) {
		t.Parallel()
		h, thread, w := inBandProfileWorker(t, config.ApprovalYolo)
		call := createInBand(createBody)
		first, err := w.inBandEvents(t.Context(), call)
		if err != nil || !first.OK {
			t.Fatalf("first dispatch = %+v, %v", first, err)
		}
		recorded := len(h.events(thread.ID()))
		second, err := w.inBandEvents(t.Context(), call)
		if err != nil {
			t.Fatal(err)
		}
		requireReplayOf(t, first, second)
		if events := h.events(thread.ID()); len(events) != recorded || h.read("n.txt") != "body" {
			t.Fatalf("replay journaled %d records and left %q, want one execution", len(events)-recorded, h.read("n.txt"))
		}
	})
}

func TestInBandReplayBypassesReachedLoopGuard(t *testing.T) {
	t.Parallel()
	h, thread, w := inBandProfileWorker(t, config.ApprovalYolo)
	var first event.ToolCompleted
	call := createInBand(createBody)
	for i := 1; i <= 4; i++ {
		call.ID = fmt.Sprintf("patch-%d", i)
		if i > 1 {
			if err := os.Remove(filepath.Join(h.cwd, "n.txt")); err != nil {
				t.Fatal(err)
			}
		}
		completed, err := w.inBandEvents(t.Context(), call)
		if err != nil || !completed.OK {
			t.Fatalf("dispatch %d = %+v, %v", i, completed, err)
		}
		first = completed
	}
	head := len(h.events(thread.ID()))
	replay, err := w.inBandEvents(t.Context(), call)
	if err != nil {
		t.Fatal(err)
	}
	requireReplayOf(t, first, replay)
	if got := len(h.events(thread.ID())); got != head {
		t.Fatalf("replay appended %d records", got-head)
	}
}

func TestInBandChangedPayloadIsInvocationMismatch(t *testing.T) {
	t.Run("REP-03/changed payload", func(t *testing.T) {
		t.Parallel()
		h, thread, w := inBandProfileWorker(t, config.ApprovalYolo)
		if first, err := w.inBandEvents(t.Context(), createInBand(createBody)); err != nil || !first.OK {
			t.Fatalf("first dispatch = %+v, %v", first, err)
		}
		recorded := len(h.events(thread.ID()))
		got, err := w.inBandEvents(t.Context(), createInBand("[n.txt#NEW]\nPUT >$:\n+other\n"))
		if err != nil || got.OK {
			t.Fatalf("mismatch dispatch = %+v, %v; want a refusal", got, err)
		}
		requireHashline(t, got.Meta, hashlineWant{"hashline-light", "rejected", "none", "invocation_mismatch", "resolve"})
		if events := h.events(thread.ID()); len(events) != recorded || h.read("n.txt") != "body" {
			t.Fatalf("mismatch journaled %d records and left %q, want no effect", len(events)-recorded, h.read("n.txt"))
		}
	})
}

func TestInBandDuplicateJoinsRunningOwner(t *testing.T) {
	t.Parallel()
	h, thread, w := inBandProfileWorker(t, config.ApprovalAsk)
	updates := thread.Subscribe()
	defer thread.Unsubscribe(updates)
	call := createInBand(createBody)
	owner := dispatchInBand(t.Context(), w, call)
	prompt := awaitApproval(t, updates)
	joined := dispatchInBand(t.Context(), w, call)
	if err := thread.Answer(prompt.RequestID, ReplyAccept); err != nil {
		t.Fatal(err)
	}
	first, second := <-owner, <-joined
	if first.err != nil || second.err != nil || !first.completed.OK {
		t.Fatalf("owner = %+v, %v; joined error %v", first.completed, first.err, second.err)
	}
	requireReplayOf(t, first.completed, second.completed)
	events := h.events(thread.ID())
	if approvalsOf(events, "p1") != 1 || startsOf(events, "p1") != 1 || h.read("n.txt") != "body" {
		t.Fatalf("approvals %d, starts %d, file %q; want one owner execution", approvalsOf(events, "p1"), startsOf(events, "p1"), h.read("n.txt"))
	}
}

func TestInBandCancelledWaiterKeepsOwnerRunning(t *testing.T) {
	t.Parallel()
	calls := newInBandCalls()
	claim := invocationClaim{identity: "thread/0/turn/1/model/p1", fingerprint: "f", profile: tool.Profile{Style: "hashline-light", Version: 1}}
	owner, held := calls.own(claim)
	waiter, joined := calls.own(claim)
	if held || !joined || waiter != owner {
		t.Fatalf("own = held %v, joined %v, same owner %v; want one owner and one waiter", held, joined, waiter == owner)
	}
	cancelled, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err := waiter.join(cancelled, claim, "p1"); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled waiter error = %v, want context.Canceled", err)
	}
	select {
	case <-owner.done:
		t.Fatal("cancelling the waiter ended the owner")
	default:
	}
	result := event.ToolCompleted{CallID: "p1", OK: true, Output: "applied\n", Meta: jsontext.Value(appliedLightMeta)}
	owner.finish(result, nil)
	got, err := waiter.join(t.Context(), claim, "p1")
	if err != nil {
		t.Fatal(err)
	}
	requireReplayOf(t, result, got)
}

func TestInBandEditFollowsEditReviewSetting(t *testing.T) {
	t.Run("enabled", func(t *testing.T) {
		t.Parallel()
		checkInBandEditReview(t, true)
	})
	t.Run("disabled", func(t *testing.T) {
		t.Parallel()
		checkInBandEditReview(t, false)
	})
}

func checkInBandEditReview(t *testing.T, enabled bool) {
	t.Helper()
	judge := &judgeStub{}
	h, thread, w := inBandProfileWorker(t, config.ApprovalYolo, func(h *harness) {
		h.cfg.EditJudge = judgeFor(judge)
		h.cfg.Config.EditReview.Enabled = enabled
	})
	if got, err := w.inBandEvents(t.Context(), createInBand(createBody)); err != nil || !got.OK {
		t.Fatalf("in-band edit = %+v, %v", got, err)
	}
	if err := w.joinReviews(t.Context()); err != nil {
		t.Fatal(err)
	}
	want := 0
	if enabled {
		want = 1
	}
	judged := judgedOf(h.events(thread.ID()))
	if len(judged) != want || len(editStatesOf(judge)) != want || (enabled && judged[0].CallID != "p1") {
		t.Fatalf("verdicts %+v and %d requests, want %d for p1", judged, len(editStatesOf(judge)), want)
	}
}

func seededInvocation(h *harness, current string, settled ...event.Event) []event.Event {
	h.write("a.txt", current)
	change := h.fileChange("a.txt", "old\n", "new\n")
	start := seededInvocationStarted(seededIdentity, "a.txt")
	return prefix(openTurn("edit"), []event.Event{assistantCall("c1", "patch", unknownRefEdit)}, approved("c1", "edit"),
		[]event.Event{startedCall("c1", "patch", "edit", unknownRefEdit, change), start}, settled)
}

func seededInvocationStarted(identity, path string) event.HashlineInvocationStarted {
	return event.HashlineInvocationStarted{
		CallID: "c1", TurnID: "t", Identity: identity, Fingerprint: "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
		Style: "hashline-light", ContractVersion: 1, Paths: []string{path},
	}
}

func TestRecoverReplaysSettledInvocation(t *testing.T) {
	t.Run("REP-01/recovery", func(t *testing.T) {
		t.Parallel()
		h := newProfileHarness(t, "hashline-light")
		settled := event.HashlineInvocationSettled{Identity: seededIdentity, OK: true, Output: "recorded output\n", Meta: jsontext.Value(appliedLightMeta)}
		h.seed(seededInvocation(h, "old\n", settled)...)
		got := completedOutputs(h.recoverSession())["c1"]
		requireReplayOf(t, event.ToolCompleted{OK: true, Output: settled.Output, Meta: settled.Meta}, got)
		if got.Recovered != recoveredReplay || h.read("a.txt") != "old\n" {
			t.Fatalf("recovered %q, file %q; want a replay without execution", got.Recovered, h.read("a.txt"))
		}
	})
}

func TestRecoverUnsettledInvocationIsIndeterminate(t *testing.T) {
	cases := []struct{ name, current, state string }{
		{"before", "old\n", tool.PathBefore},
		{"after", "new\n", tool.PathAfter},
		{"foreign", "other\n", tool.PathUnknown},
	}
	for _, test := range cases {
		t.Run("REP-04/"+test.name, func(t *testing.T) {
			t.Parallel()
			h := newProfileHarness(t, "hashline-light")
			h.seed(seededInvocation(h, test.current)...)
			got := completedOutputs(h.recoverSession())["c1"]
			meta := decodeHashline(t, got.Meta)
			requireHashlineHead(t, got.Meta, meta, hashlineWant{"hashline-light", "indeterminate", "unknown", "commit_unknown", "io"})
			if got.OK || got.Recovered != recoveredStopped || len(meta.PathStates) != 1 || meta.PathStates[0].Path != "a.txt" || meta.PathStates[0].State != test.state {
				t.Fatalf("completion ok=%v recovered=%q meta %s, want indeterminate with a.txt %s", got.OK, got.Recovered, got.Meta, test.state)
			}
			if h.read("a.txt") != test.current {
				t.Fatalf("file = %q, want %q untouched", h.read("a.txt"), test.current)
			}
		})
	}
}

func TestRecoverReusedCallIDReportsOnlyCurrentInvocationPaths(t *testing.T) {
	t.Parallel()
	h := newProfileHarness(t, "hashline-light")
	settled := event.HashlineInvocationSettled{Identity: seededIdentity, OK: true, Output: "recorded output\n", Meta: jsontext.Value(appliedLightMeta)}
	previous := seededInvocation(h, "new\n", settled, event.ToolCompleted{CallID: "c1", OK: true, Recovered: "none"})
	h.write("b.txt", "new\n")
	args := strings.Replace(unknownRefEdit, "a.txt", "b.txt", 1)
	assistant := assistantCall("c1", "patch", args)
	assistant.ItemID = "a-second"
	assistant.ContextSeq = 10
	h.seed(append(previous,
		assistant,
		event.ApprovalRequested{RequestID: "r-second", CallID: "c1", Summary: "edit b.txt", Class: "edit"},
		event.ApprovalResolved{RequestID: "r-second", Decision: "accept", By: "mode:auto"},
		startedCall("c1", "patch", "edit", args, h.fileChange("b.txt", "old\n", "new\n")),
		seededInvocationStarted("crashed/3/t/10/model/c1", "b.txt"),
	)...)
	got := completedOutputs(h.recoverSession())["c1"]
	meta := decodeHashline(t, got.Meta)
	requireHashlineHead(t, got.Meta, meta, hashlineWant{"hashline-light", "indeterminate", "unknown", "commit_unknown", "io"})
	if len(meta.PathStates) != 1 || meta.PathStates[0].Path != "b.txt" || meta.PathStates[0].State != tool.PathAfter {
		t.Fatalf("indeterminate paths = %+v; want only b.txt after", meta.PathStates)
	}
}

func TestRecoverRefusesPendingInvocationFromEarlierOpen(t *testing.T) {
	t.Run("REP-07/pending call from an earlier open", func(t *testing.T) {
		t.Parallel()
		h := newProfileHarness(t, "hashline-light")
		h.seed(prefix(openTurn("edit"), []event.Event{
			assistantCall("c1", "patch", createCall),
			event.ApprovalRequested{RequestID: "r-c1", CallID: "c1", Summary: "edit", Class: "edit"},
		})...)
		events := h.recoverSession()
		got := completedOutputs(events)["c1"]
		requireHashline(t, got.Meta, hashlineWant{"hashline-light", "needs_context", "none", "reference_expired", "proof"})
		if got.OK || started(events, "c1") || len(invocationLogOf(events).started) != 0 {
			t.Fatalf("completion = %+v, started %v; want a refusal without execution", got, started(events, "c1"))
		}
		if _, err := os.Stat(filepath.Join(h.cwd, "n.txt")); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("n.txt exists after refusal: %v", err)
		}
	})
}

func TestInvocationFingerprintBindsTuple(t *testing.T) {
	t.Run("C16/fingerprint tuple", func(t *testing.T) {
		t.Parallel()
		profile := tool.Profile{Style: "hashline-light", Version: 1}
		payload := []byte(createCall)
		want := specFingerprint("hashline-light", "/workspace", "model", createCall)
		if got := invocationFingerprint(profile, "/workspace", "model", payload); got != want {
			t.Fatalf("fingerprint = %s, want %s", got, want)
		}
		for name, got := range map[string]string{
			"style":     invocationFingerprint(tool.Profile{Style: "hashline-enhanced", Version: 1}, "/workspace", "model", payload),
			"version":   invocationFingerprint(tool.Profile{Style: "hashline-light", Version: 2}, "/workspace", "model", payload),
			"workspace": invocationFingerprint(profile, "/other", "model", payload),
			"consumer":  invocationFingerprint(profile, "/workspace", "evaluator", payload),
			"payload":   invocationFingerprint(profile, "/workspace", "model", append(payload, ' ')),
		} {
			if got == want {
				t.Fatalf("changing the %s kept the fingerprint", name)
			}
		}
	})
}
