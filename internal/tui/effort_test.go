package tui

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/rpc"
)

type lifecycleTurn struct {
	text, effort, wantEffort, glyph, state string
}

func TestEffortGlyphFollowsCapturedRequestLifecycle(t *testing.T) {
	ctx, cancel := context.WithTimeoutCause(t.Context(), 10*time.Second, errors.New("effort lifecycle timed out"))
	t.Cleanup(cancel)
	wire := startEffortWire(ctx, t)
	m := wire.openThread(ctx, t)
	cases := []lifecycleTurn{
		{text: "ultrathink about this probe", wantEffort: "max", glyph: "⣿", state: stateIdle},
		{text: "plain effort probe", wantEffort: "low", glyph: "⣀", state: stateIdle},
		{text: "interrupt effort probe", effort: "max", wantEffort: "max", glyph: "⣿", state: stateInterrupted},
	}
	var delayed resultMsg
	for i, test := range cases {
		turn := runLifecycleTurn(ctx, t, wire, &m, test)
		switch i {
		case 0:
			delayed = resumeActiveTurn(ctx, t, &m, test.wantEffort)
			wire.completeTurn(m.thread.id, turn.ID)
		case 1:
			applyStaleResume(t, &m, delayed, turn.ID, test.wantEffort)
			wire.completeTurn(m.thread.id, turn.ID)
		case 2:
			wire.interruptTurn(ctx, t, m.thread.id, turn.ID)
		}
		wire.applyUntil(ctx, t, &m, "turn/completed")
		assertBaseline(t, &m, i, test.state)
	}
}

func runLifecycleTurn(ctx context.Context, t *testing.T, wire *effortWire, m *model, test lifecycleTurn) rpc.Turn {
	t.Helper()
	var started rpc.TurnStartResponse
	wire.call(ctx, t, "turn/start", rpc.TurnStartParams{
		ThreadID: m.thread.id,
		Effort:   test.effort,
		Input:    []rpc.InputItem{{Type: "text", Text: test.text}},
	}, &started)
	wire.applyUntil(ctx, t, m, "dalgona/turn/effort")
	if effortShown(m) != test.wantEffort || !strings.Contains(bandRowOf(*m), test.glyph) {
		t.Fatalf("running effort %q, band %q; want %q with %s", effortShown(m), bandRowOf(*m), test.wantEffort, test.glyph)
	}
	return started.Turn
}

func effortShown(m *model) string {
	if m.thread.turnEffort == nil {
		return ""
	}
	return *m.thread.turnEffort
}

func resumeActiveTurn(ctx context.Context, t *testing.T, m *model, wantEffort string) resultMsg {
	t.Helper()
	applied := invokeResume(ctx, t, m)
	resumed, ok := applied.value.(*resumedThread)
	if !ok || resumed.turnEffort == nil || *resumed.turnEffort != wantEffort {
		t.Fatalf("active resume result = %+v, want turn effort %q", applied, wantEffort)
	}
	m.applyResumed(applied.value)
	if !strings.Contains(bandRowOf(*m), "⣿") {
		t.Fatalf("resume lost the active turn glyph: %q", bandRowOf(*m))
	}
	return invokeResume(ctx, t, m)
}

func invokeResume(ctx context.Context, t *testing.T, m *model) resultMsg {
	t.Helper()
	return invokeResumeOf(ctx, t, m, m.thread.id)
}

func invokeResumeOf(ctx context.Context, t *testing.T, m *model, threadID string) resultMsg {
	t.Helper()
	msg, ok := m.resumeThread(threadID)().(resultMsg)
	if !ok || msg.op != "resumed" || msg.err != nil {
		t.Fatalf("resume command result: %+v", msg)
	}
	return msg
}

func applyStaleResume(t *testing.T, m *model, delayed resultMsg, turnID, wantEffort string) {
	t.Helper()
	m.applyResumed(delayed.value)
	if m.thread.turnID != turnID || effortShown(m) != wantEffort || !strings.Contains(bandRowOf(*m), "⣀") {
		t.Fatalf("stale resume overwrote turn %s: turn=%s effort=%v band=%q", turnID, m.thread.turnID, m.thread.turnEffort, bandRowOf(*m))
	}
}

func assertBaseline(t *testing.T, m *model, index int, wantState string) {
	t.Helper()
	if m.thread.state != wantState || m.thread.turnEffort != nil || m.thread.effort != wireConfiguredEffort || !strings.Contains(bandRowOf(*m), "⣀") {
		t.Fatalf("turn %d did not restore the %s baseline: state=%q effort=%v band=%q", index, wantState, m.thread.state, m.thread.turnEffort, bandRowOf(*m))
	}
}

func TestResumeReplaysHeldTargetNotifications(t *testing.T) {
	ctx, cancel := context.WithTimeoutCause(t.Context(), 15*time.Second, errors.New("resume replay timed out"))
	t.Cleanup(cancel)
	wire := startEffortWire(ctx, t)
	m := wire.openThread(ctx, t)
	current := m.thread.id
	target := wire.spawnThread(ctx, t, &m)
	held := invokeResumeOf(ctx, t, &m, target.ID)
	var turn rpc.TurnStartResponse
	wire.call(ctx, t, "turn/start", rpc.TurnStartParams{
		ThreadID: target.ID,
		Effort:   "max",
		Input:    []rpc.InputItem{{Type: "text", Text: "post snapshot probe"}},
	}, &turn)
	wire.consumeUntil(ctx, t, &m, "turn/started")
	wire.consumeUntil(ctx, t, &m, "dalgona/turn/effort")
	wire.emitDelta(target.ID)
	wire.consumeUntil(ctx, t, &m, "item/agentMessage/delta")
	assertPendingHoldsTarget(t, &m, current)
	m.applyNotification(notificationMsg{
		method: "thread/name/updated",
		params: []byte(`{"threadId":"` + current + `","threadName":"alpha"}`),
	})
	if !strings.Contains(m.thread.title, "alpha") {
		t.Fatalf("current-thread notification dropped during pending resume: title %q", m.thread.title)
	}
	m.applyResult(held)
	assertReplayedResume(t, &m, target.ID, turn.Turn.ID)
}

func assertPendingHoldsTarget(t *testing.T, m *model, current string) {
	t.Helper()
	if m.thread.id != current || m.thread.turnID != "" {
		t.Fatalf("pending resume applied target state early: thread %q turn %q", m.thread.id, m.thread.turnID)
	}
	if strings.Contains(bandRowOf(*m), "⣿") {
		t.Fatalf("pending resume leaked the target effort glyph: %q", bandRowOf(*m))
	}
}

func assertReplayedResume(t *testing.T, m *model, threadID, turnID string) {
	t.Helper()
	if m.thread.id != threadID {
		t.Fatalf("resume did not switch to %q: %q", threadID, m.thread.id)
	}
	if m.thread.turnID != turnID {
		t.Fatalf("replayed turn/started lost the newest turn: %q want %q", m.thread.turnID, turnID)
	}
	if effortShown(m) != "max" {
		t.Fatalf("replayed effort lost: %v", m.thread.turnEffort)
	}
	if !strings.Contains(bandRowOf(*m), "⣿") {
		t.Fatalf("effort glyph not restored after replay: %q", bandRowOf(*m))
	}
	if !liveItemContains(*m, "partial") {
		t.Fatalf("replayed delta did not restore the live item: %+v", m.live.items)
	}
	if m.thread.state != stateRunning {
		t.Fatalf("state = %q, want running", m.thread.state)
	}
}

func liveItemContains(m model, fragment string) bool {
	for _, item := range m.live.items {
		if item.Type == "agentMessage" && strings.Contains(string(item.text), fragment) {
			return true
		}
	}
	return false
}

func TestResumeLatestRequestSupersedesPending(t *testing.T) {
	m := bandModel("/work")
	m.thread.id = "a"
	m.resumeThread("b")
	m.resumeThread("c")
	m.applyResumed(&resumedThread{ThreadReadResponse: rpc.ThreadReadResponse{Thread: rpc.Thread{ID: "b"}}, resumeSeq: 1})
	if m.thread.id != "a" {
		t.Fatalf("stale resume switched thread to %q", m.thread.id)
	}
	if m.pending == nil || m.pending.target != "c" {
		t.Fatalf("stale resume cleared the newer pending target: %+v", m.pending)
	}
	m.applyResult(resultMsg{op: "resumed", seq: 1, err: errors.New("stale failure")})
	if m.pending == nil || m.pending.target != "c" {
		t.Fatalf("stale resume error cleared the newer pending target: %+v", m.pending)
	}
	m.applyResumed(&resumedThread{ThreadReadResponse: rpc.ThreadReadResponse{Thread: rpc.Thread{ID: "c", Name: "see"}}, resumeSeq: 2})
	if m.thread.id != "c" {
		t.Fatalf("newest resume did not apply: %q", m.thread.id)
	}
}

func TestResumeErrorPreservesThread(t *testing.T) {
	m := bandModel("/work")
	m.thread.id = "a"
	m.resumeThread("b")
	m.applyResult(resultMsg{op: "resumed", seq: 1, err: errors.New("boom")})
	if m.thread.id != "a" {
		t.Fatalf("failed resume switched thread: %q", m.thread.id)
	}
	if m.pending == nil || m.pending.target != "" {
		t.Fatalf("failed resume left pending armed: %+v", m.pending)
	}
	if !strings.Contains(m.notice, "resumed failed") {
		t.Fatalf("failure notice missing: %q", m.notice)
	}
}

func TestResumePendingOverflowPreservesThread(t *testing.T) {
	m := bandModel("/work")
	m.thread.id = "a"
	m.resumeThread("b")
	params := []byte(`{"threadId":"b","turnId":"t1","effort":"high"}`)
	for range pendingResumeLimit {
		m.applyNotification(notificationMsg{method: "dalgona/turn/effort", params: params})
	}
	m.applyNotification(notificationMsg{method: "dalgona/turn/effort", params: params})
	if m.thread.id != "a" {
		t.Fatalf("overflow switched thread: %q", m.thread.id)
	}
	if m.pending == nil || !m.pending.failed {
		t.Fatalf("overflow did not fail the pending resume: %+v", m.pending)
	}
	if !strings.Contains(m.notice, "resume to b aborted") {
		t.Fatalf("overflow notice missing: %q", m.notice)
	}
	m.applyResumed(&resumedThread{ThreadReadResponse: rpc.ThreadReadResponse{Thread: rpc.Thread{ID: "b"}}, resumeSeq: 1})
	if m.thread.id != "a" {
		t.Fatalf("failed resume installed the snapshot on %q", m.thread.id)
	}
	if m.pending == nil || m.pending.target != "" {
		t.Fatalf("failed resume did not clear pending: %+v", m.pending)
	}
}
