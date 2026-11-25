package agent

import (
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

type invocationLog struct {
	started    []event.HashlineInvocationStarted
	settled    []event.HashlineInvocationSettled
	startedAt  []int
	settledAt  []int
	toolStart  map[string]int
	toolFinish map[string]int
}

func invocationLogOf(events []event.Seqd) invocationLog {
	log := invocationLog{toolStart: make(map[string]int), toolFinish: make(map[string]int)}
	for i, item := range events {
		switch value := item.Event.(type) {
		case event.HashlineInvocationStarted:
			log.started, log.startedAt = append(log.started, value), append(log.startedAt, i)
		case event.HashlineInvocationSettled:
			log.settled, log.settledAt = append(log.settled, value), append(log.settledAt, i)
		case event.ToolStarted:
			log.toolStart[value.CallID] = i
		case event.ToolCompleted:
			log.toolFinish[value.CallID] = i
		}
	}
	return log
}

func specFingerprint(style, workspace, consumer, payload string) string {
	var framed []byte
	for _, part := range []string{style, "1", workspace, consumer, payload} {
		framed = binary.BigEndian.AppendUint64(framed, uint64(len(part)))
		framed = append(framed, part...)
	}
	sum := sha256.Sum256(framed)
	return hex.EncodeToString(sum[:])
}

func replayedState(t *testing.T, events []event.Seqd) event.State {
	t.Helper()
	var state event.State
	for _, item := range events {
		if err := state.Apply(item.Seq, item.Event); err != nil {
			t.Fatalf("replay of seq %d (%s): %v", item.Seq, item.Event.Type(), err)
		}
	}
	return state
}

func requireSettlementMatches(t *testing.T, settled event.HashlineInvocationSettled, completed event.ToolCompleted) {
	t.Helper()
	sameSpill := (settled.Spill == nil) == (completed.Spill == nil) && (settled.Spill == nil || *settled.Spill == *completed.Spill)
	if settled.OK != completed.OK || settled.Output != completed.Output || !sameSpill || !bytes.Equal(settled.Meta, completed.Meta) {
		t.Fatalf("settled ok=%v spill=%v meta=%s, want ok=%v spill=%v meta=%s and equal output", settled.OK, settled.Spill, settled.Meta, completed.OK, completed.Spill, completed.Meta)
	}
}

func requireOneLifecycle(t *testing.T, h *harness, events []event.Seqd, callID, style string) event.HashlineInvocationStarted {
	t.Helper()
	log := invocationLogOf(events)
	if len(log.started) != 1 || len(log.settled) != 1 {
		t.Fatalf("invocation records: %d started, %d settled; want one of each", len(log.started), len(log.settled))
	}
	started, settled := log.started[0], log.settled[0]
	toolStart, toolFinish := lifecycleBounds(t, log, callID)
	toolStarted := events[toolStart].Event.(event.ToolStarted)
	requireStartedFields(t, started, toolStarted, callID, style)
	requireStartedFingerprint(t, h.cwd, toolStarted, started, style)
	if settled.Identity != started.Identity {
		t.Fatalf("settled identity %q, want %q", settled.Identity, started.Identity)
	}
	requireSettlementMatches(t, settled, events[toolFinish].Event.(event.ToolCompleted))
	return started
}

func lifecycleBounds(t *testing.T, log invocationLog, callID string) (int, int) {
	t.Helper()
	toolStart, toolFinish := log.toolStart[callID], log.toolFinish[callID]
	if toolStart >= log.startedAt[0] || log.startedAt[0] >= log.settledAt[0] || log.settledAt[0] >= toolFinish {
		t.Fatalf("order: tool.started %d, invocation started %d, settled %d, tool.completed %d", toolStart, log.startedAt[0], log.settledAt[0], toolFinish)
	}
	return toolStart, toolFinish
}

func requireStartedFields(t *testing.T, started event.HashlineInvocationStarted, toolStarted event.ToolStarted, callID, style string) {
	t.Helper()
	if started.CallID != callID || started.TurnID != toolStarted.TurnID || started.Style != style || started.ContractVersion != 1 || !slices.Equal(started.Paths, []string{"a.txt"}) {
		t.Fatalf("started = %+v, want call %s turn %s style %s version 1 paths [a.txt]", started, callID, toolStarted.TurnID, style)
	}
}

func requireStartedFingerprint(t *testing.T, cwd string, toolStarted event.ToolStarted, started event.HashlineInvocationStarted, style string) {
	t.Helper()
	if want := specFingerprint(style, cwd, "model", string(toolStarted.Args)); started.Fingerprint != want {
		t.Fatalf("fingerprint = %s, want %s", started.Fingerprint, want)
	}
}

func TestProfileEditJournalsOneInvocationLifecycle(t *testing.T) {
	cases := []struct {
		name, style, edit, file string
	}{
		{"applied light", "hashline-light", editTwo, "one\nTWO\nthree\n"},
		{"no_change light", "hashline-light", "[a.txt@{ref:a.txt}]\nPUT 2:\n+two\n", threeLines},
		{"applied enhanced", "hashline-enhanced", editTwo, "one\nTWO\nthree\n"},
	}
	for _, test := range cases {
		t.Run("C16/"+test.name, func(t *testing.T) {
			t.Parallel()
			h := newProfileHarness(t, test.style)
			h.write("a.txt", threeLines)
			events := h.runRounds(calls(readCall("r1", `{"path":"a.txt"}`)), calls(patchCall("p1", test.edit)))
			if completed := completedOutputs(events)["p1"]; !completed.OK || h.read("a.txt") != test.file {
				t.Fatalf("completion = %+v, file %q; want ok and %q", completed, h.read("a.txt"), test.file)
			}
			started := requireOneLifecycle(t, h, events, "p1", test.style)
			log := invocationLogOf(events)
			want := event.InvocationState{
				CallID: "p1", TurnID: started.TurnID, Fingerprint: started.Fingerprint,
				StartedSeq: events[log.startedAt[0]].Seq, SettledSeq: events[log.settledAt[0]].Seq,
			}
			if got := replayedState(t, events).Invocations[started.Identity]; got != want {
				t.Fatalf("replayed invocation = %+v, want %+v", got, want)
			}
		})
	}
}

func TestInvocationSettlementKeepsSpillOfLongOutput(t *testing.T) {
	t.Parallel()
	h := newProfileHarness(t, "hashline-light")
	line := strings.Repeat("x", 60)
	input := "[big.txt#NEW]\nPUT >$:\n" + strings.Repeat("+"+line+"\n", 1500)
	events := h.runRounds(calls(patchCall("p1", input)))
	completed := completedOutputs(events)["p1"]
	log := invocationLogOf(events)
	if !completed.OK || completed.Spill == nil || len(log.settled) != 1 {
		t.Fatalf("completion ok=%v spill=%v with %d settled records; want ok, spilled, one settlement", completed.OK, completed.Spill, len(log.settled))
	}
	settled := log.settled[0]
	requireSettlementMatches(t, settled, completed)
	sha, _ := strings.CutPrefix(settled.Spill.Path, "blobs/")
	data, err := h.journal.Blob(sha)
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(data)
	if settled.Spill.SHA256 != hex.EncodeToString(sum[:]) || settled.Spill.Bytes != int64(len(data)) || len(data) <= event.MaxInlineBytes {
		t.Fatalf("spill = %+v over %d stored bytes, want the digest of output above %d bytes", *settled.Spill, len(data), event.MaxInlineBytes)
	}
	if !strings.Contains(string(data), "1500:"+line+"\n") {
		t.Fatal("spilled output does not hold the last applied line")
	}
	replayedState(t, events)
}

func TestRepeatedCallIDGetsNewInvocationIdentity(t *testing.T) {
	t.Run("REP-06/repeated call id in different rounds", func(t *testing.T) {
		t.Parallel()
		h := newProfileHarness(t, "hashline-light")
		events := h.runRounds(
			calls(patchCall("p1", "[n1.txt#NEW]\nPUT >$:\n+one\n")),
			calls(patchCall("p1", "[n2.txt#NEW]\nPUT >$:\n+two\n")),
		)
		log := invocationLogOf(events)
		if len(log.started) != 2 || len(log.settled) != 2 || log.started[0].Identity == log.started[1].Identity {
			t.Fatalf("started = %+v, settled %d; want two settled invocations with distinct identities", log.started, len(log.settled))
		}
		if h.read("n1.txt") != "one" || h.read("n2.txt") != "two" {
			t.Fatalf("files = %q, %q; want both edits applied", h.read("n1.txt"), h.read("n2.txt"))
		}
		replayedState(t, events)
	})
}

func TestSettledProfileEditFollowsEditReviewSetting(t *testing.T) {
	for _, enabled := range []bool{true, false} {
		t.Run(fmt.Sprintf("enabled=%t", enabled), func(t *testing.T) {
			t.Parallel()
			judge := &judgeStub{}
			h := newProfileHarness(t, "hashline-light", func(h *harness) {
				h.cfg.EditJudge = judgeFor(judge)
				h.cfg.Config.EditReview.Enabled = enabled
			})
			events := h.runRounds(calls(patchCall("p1", "[n.txt#NEW]\nPUT >$:\n+body\n")))
			if log := invocationLogOf(events); len(log.settled) != 1 || h.read("n.txt") != "body" {
				t.Fatalf("settled records = %d, file %q; want one settled edit of n.txt", len(log.settled), h.read("n.txt"))
			}
			want := 0
			if enabled {
				want = 1
			}
			requireReviewCounts(t, events, judge, want, want)
			if judged := judgedOf(events); enabled && (judged[0].CallID != "p1" || !judgedAfterCompletion(events, "p1")) {
				t.Fatalf("judged = %+v, want a verdict on p1 after its completion", judged[0])
			}
		})
	}
}
