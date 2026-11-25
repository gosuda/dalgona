package rpcserver

import (
	"cmp"
	json "encoding/json/v2"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
)

func journalRecords(t *testing.T, r *rig, threadID string) []event.Seqd {
	t.Helper()
	thread, err := r.host.Resume(t.Context(), threadID)
	if err != nil {
		t.Fatal(err)
	}
	records, _, err := thread.Records()
	if err != nil {
		t.Fatal(err)
	}
	return records
}

func journaledMessage(t *testing.T, r *rig, threadID, operationID string) event.UserMessage {
	t.Helper()
	for _, record := range journalRecords(t, r, threadID) {
		if message, ok := record.Event.(event.UserMessage); ok && message.Operation == operationID {
			return message
		}
	}
	t.Fatalf("thread %s journaled no user message for operation %s", threadID, operationID)
	return event.UserMessage{}
}

func TestReceiptCoveredEffectsJournalTheirOperationID(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	opened := rpc.NewOperationID()
	var thread rpc.ThreadStartResponse
	c.ok("thread/start", map[string]string{"cwd": r.cwd, rpc.OperationMember: opened}, &thread)
	threadID := thread.Thread.ID
	if created, ok := sessionCreated(journalRecords(t, r, threadID)); !ok || created.Operation != opened {
		t.Fatalf("session.created = %+v, want operation %s", created, opened)
	}
	started := rpc.NewOperationID()
	var response rpc.TurnStartResponse
	c.ok("turn/start", operationPrompt(threadID, "journal me", started), &response)
	c.awaitEvent("turn/completed", nil)
	if message := journaledMessage(t, r, threadID, started); message.TurnID != response.Turn.ID {
		t.Fatalf("journaled turn %s, response turn %s", message.TurnID, response.Turn.ID)
	}
	named := rpc.NewOperationID()
	c.ok("thread/name/set", map[string]string{"threadId": threadID, "name": "renamed", rpc.OperationMember: named}, nil)
	for _, record := range journalRecords(t, r, threadID) {
		if rename, ok := record.Event.(event.ThreadNamed); ok && rename.Operation == named && rename.Name == "renamed" {
			return
		}
	}
	t.Fatalf("thread %s journaled no rename for operation %s", threadID, named)
}

func writeInterruptedReceipt(t *testing.T, home string, rec receipt) {
	t.Helper()
	rec.Digest, rec.ExpiresAt = cmp.Or(rec.Digest, "digest"), time.Now().Add(receiptLifetime)
	raw, err := json.Marshal(rec)
	if err != nil {
		t.Fatal(err)
	}
	dir := filepath.Join(home, receiptDirName)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, rec.OperationID+receiptSuffix), raw, 0o600); err != nil {
		t.Fatal(err)
	}
}

func requireRebuiltTurn(t *testing.T, r *rig, c *nativeClient, threadID, operationID string) {
	t.Helper()
	var recovered rpc.TurnStartResponse
	record := c.operation(operationID)
	if err := json.Unmarshal(record.Result, &recovered); err != nil || record.State != stateSucceeded {
		t.Fatalf("journaled turn/start receipt = %+v (%v), want succeeded", record, err)
	}
	if want := journaledMessage(t, r, threadID, operationID).TurnID; recovered.Turn.ID != want || recovered.Turn.Status != rpc.StatusInProgress {
		t.Fatalf("rebuilt turn = %+v, want %s inProgress from the journal", recovered.Turn, want)
	}
}

func TestInterruptedThreadReceiptsSettleFromTheJournal(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	threadID := c.startThread(r)
	thread, err := r.host.Resume(t.Context(), threadID)
	if err != nil {
		t.Fatal(err)
	}
	started, named, plain := rpc.NewOperationID(), rpc.NewOperationID(), rpc.NewOperationID()
	if err := thread.Submit(agent.WithOperation(t.Context(), started), agent.Input{Text: "crashed before its receipt"}); err != nil {
		t.Fatal(err)
	}
	c.awaitEvent("turn/completed", nil)
	if err := thread.SetName(agent.WithOperation(t.Context(), named), "crash name"); err != nil {
		t.Fatal(err)
	}
	writeInterruptedReceipt(t, r.home, receipt{OperationID: started, Method: "turn/start", ThreadID: threadID, State: stateRunning})
	writeInterruptedReceipt(t, r.home, receipt{OperationID: named, Method: "thread/name/set", ThreadID: threadID, State: stateReserved})
	writeInterruptedReceipt(t, r.home, receipt{OperationID: plain, Method: "config/value/write", State: stateRunning})
	lost := lostEffect("turn/start", threadID)
	crashBeforeReceipts(t, r.home, lost)
	requireRebuiltTurn(t, r, c, threadID, started)
	if renamed := c.operation(named); renamed.State != stateSucceeded || string(renamed.Result) != "{}" {
		t.Fatalf("journaled rename receipt = %+v, want succeeded with {}", renamed)
	}
	requireCancelled(t, lost, c.operation(lost.operationID))
	if unbound := c.operation(plain); unbound.State != stateUnknown {
		t.Fatalf("receipt without a journal binding = %+v, want unknown", unbound)
	}
}

func TestInterruptedThreadStartSettlesFromTheJournal(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	opened, lost := rpc.NewOperationID(), rpc.NewOperationID()
	thread, err := r.host.Start(agent.WithOperation(t.Context(), opened), agent.ThreadSpec{Cwd: r.cwd})
	if err != nil {
		t.Fatal(err)
	}
	writeInterruptedReceipt(t, r.home, receipt{OperationID: opened, Method: methodThreadStart, State: stateRunning})
	writeInterruptedReceipt(t, r.home, receipt{OperationID: lost, Method: methodThreadStart, State: stateReserved})
	var record rpc.OperationRecord
	c.ok("dalgona/operation/read", map[string]string{"operationId": opened}, &record)
	var recovered rpc.ThreadStartResponse
	if err := json.Unmarshal(record.Result, &recovered); err != nil || record.State != stateSucceeded || recovered.Thread.ID != thread.ID() {
		t.Fatalf("journaled thread/start receipt = %+v (%v), want succeeded for thread %s", record, err, thread.ID())
	}
	record = rpc.OperationRecord{}
	c.ok("dalgona/operation/read", map[string]string{"operationId": lost}, &record)
	if record.State != stateFailed || record.Error == nil {
		t.Fatalf("unjournaled thread/start receipt = %+v, want failed", record)
	}
	if data, ok := rpc.ErrorDataOf(record.Error); !ok || data.Kind != rpc.ErrorCancelled {
		t.Fatalf("unjournaled thread/start error = %+v, want cancelled", record.Error)
	}
}
