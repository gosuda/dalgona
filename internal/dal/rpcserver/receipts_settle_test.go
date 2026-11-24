package rpcserver

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
)

type journaledCase struct {
	method      string
	threadID    string
	operationID string
	result      jsontext.Value
}

func (c *nativeClient) effect(method, threadID string, params map[string]any) journaledCase {
	c.t.Helper()
	operationID := rpc.NewOperationID()
	params["threadId"], params[rpc.OperationMember] = threadID, operationID
	m := c.call(method, params)
	if m.Error != nil {
		c.t.Fatalf("%s failed: %d %s", method, m.Error.Code, m.Error.Message)
	}
	return journaledCase{method: method, threadID: threadID, operationID: operationID, result: m.Result}
}

func lostEffect(method, threadID string) journaledCase {
	return journaledCase{method: method, threadID: threadID, operationID: rpc.NewOperationID()}
}

func crashBeforeReceipts(t *testing.T, home string, cases ...journaledCase) {
	t.Helper()
	for _, item := range cases {
		writeInterruptedReceipt(t, home, receipt{OperationID: item.operationID, Method: item.method, ThreadID: item.threadID, State: stateReserved})
	}
	receiptStoresMu.Lock()
	delete(receiptStores, filepath.Join(home, receiptDirName))
	receiptStoresMu.Unlock()
}

func (c *nativeClient) operation(id string) rpc.OperationRecord {
	c.t.Helper()
	var record rpc.OperationRecord
	c.ok("dalgona/operation/read", map[string]string{"operationId": id}, &record)
	return record
}

func canonicalJSON(t *testing.T, raw jsontext.Value) string {
	t.Helper()
	canonical := jsontext.Value(bytes.Clone(raw))
	if err := canonical.Canonicalize(); err != nil {
		t.Fatalf("canonicalize %s: %v", raw, err)
	}
	return string(canonical)
}

func requireCancelled(t *testing.T, item journaledCase, record rpc.OperationRecord) {
	t.Helper()
	if record.State != stateFailed || record.Error == nil {
		t.Fatalf("%s receipt without its journal effect = %s %s, want failed with cancelled", item.method, record.State, record.Result)
	}
	if data, ok := rpc.ErrorDataOf(record.Error); !ok || data.Kind != rpc.ErrorCancelled || data.OperationID != item.operationID {
		t.Fatalf("%s receipt error = %+v, want cancelled for %s", item.method, record.Error, item.operationID)
	}
}

func requireSettledFromJournal(t *testing.T, r *rig, c *nativeClient, cases ...journaledCase) {
	t.Helper()
	crashBeforeReceipts(t, r.home, cases...)
	for _, item := range cases {
		record := c.operation(item.operationID)
		if item.result == nil {
			requireCancelled(t, item, record)
			continue
		}
		if record.State != stateSucceeded || canonicalJSON(t, record.Result) != canonicalJSON(t, item.result) {
			t.Fatalf("%s receipt after a crash = %s %s, want succeeded with %s", item.method, record.State, record.Result, item.result)
		}
	}
}

func TestInterruptedArchiveReceiptsSettleFromTheJournal(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	hooked := r.connectNative(ServeOptions{BeforeArchive: func(context.Context, string) error { return errors.New("box busy") }})
	archivedID, restoredID, unarchivedID := c.startThread(r), c.startThread(r), c.startThread(r)
	c.ok("thread/archive", map[string]any{"threadId": unarchivedID}, nil)
	restored := journaledCase{method: "thread/archive", threadID: restoredID, operationID: rpc.NewOperationID()}
	if m := hooked.call(restored.method, map[string]any{"threadId": restoredID, rpc.OperationMember: restored.operationID}); m.Error == nil {
		t.Fatalf("archive under a failing hook = %s, want an error", m.Result)
	}
	archived := c.effect("thread/archive", archivedID, map[string]any{})
	unarchived := c.effect("thread/unarchive", unarchivedID, map[string]any{})
	requireSettledFromJournal(t, r, c, archived, unarchived, restored,
		lostEffect("thread/archive", archivedID), lostEffect("thread/unarchive", unarchivedID))
}

func TestInterruptedBranchAndForkReceiptsSettleFromTheJournal(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	threadID := c.startThread(r)
	turns := make([]string, 0, 2)
	for _, text := range []string{"one", "two"} {
		var started rpc.TurnStartResponse
		c.ok("turn/start", prompt(threadID, text), &started)
		c.awaitEvent("turn/completed", nil)
		turns = append(turns, started.Turn.ID)
	}
	leaf := c.effect("dalgona/thread/leaf/set", threadID, map[string]any{"turnId": turns[1]})
	reverted := c.effect("thread/revert", threadID, map[string]any{"beforeTurnId": turns[1]})
	forked := c.effect("thread/fork", threadID, map[string]any{})
	requireSettledFromJournal(t, r, c, leaf, reverted, forked,
		lostEffect(leaf.method, threadID), lostEffect(reverted.method, threadID), lostEffect(forked.method, threadID))
}

func TestInterruptedCompactReceiptSettlesFromTheJournal(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	r.cfg.Config.CompactionKeepTokens = 1
	r.openHost()
	c := r.connectNative(ServeOptions{})
	threadID := c.startThread(r)
	for _, text := range []string{"one", "two"} {
		r.llm.push(llmReply{body: strings.Replace(sseText("answer "+text), `"input_tokens":20`, `"input_tokens":20000`, 1)})
		c.ok("turn/start", prompt(threadID, text), nil)
		c.awaitEvent("turn/completed", nil)
	}
	compacted := c.effect("thread/compact/start", threadID, map[string]any{})
	requireSettledFromJournal(t, r, c, compacted, lostEffect(compacted.method, threadID))
}

func TestInterruptedTurnControlReceiptsSettleFromTheJournal(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	threadID := c.startThread(r)
	reviewed := c.effect("review/start", threadID, map[string]any{"target": map[string]any{"type": "uncommittedChanges"}})
	c.awaitEvent("turn/completed", nil)
	gate := make(chan struct{})
	t.Cleanup(func() { close(gate) })
	r.llm.push(llmReply{body: sseText("slow"), gate: gate})
	var started rpc.TurnStartResponse
	c.ok("turn/start", prompt(threadID, "slow"), &started)
	interrupted := c.effect("turn/interrupt", threadID, map[string]any{"turnId": started.Turn.ID})
	c.awaitEvent("turn/completed", nil)
	requireSettledFromJournal(t, r, c, reviewed, interrupted,
		lostEffect(reviewed.method, threadID), lostEffect(interrupted.method, threadID))
}

func TestInterruptedAdvisorReceiptsStayUnknown(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	threadID := c.startThread(r)
	cases := []journaledCase{lostEffect("dalgona/advisor/set", threadID), lostEffect("dalgona/advisor/reset", threadID)}
	crashBeforeReceipts(t, r.home, cases...)
	for _, item := range cases {
		if record := c.operation(item.operationID); record.State != stateUnknown {
			t.Fatalf("%s receipt after a crash = %+v, want unknown", item.method, record)
		}
	}
}
