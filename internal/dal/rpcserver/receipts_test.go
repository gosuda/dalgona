package rpcserver

import (
	json "encoding/json/v2"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func (f *fakeLLM) requests() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.count
}

func operationPrompt(threadID, text, operationID string) map[string]any {
	params := prompt(threadID, text)
	params[rpc.OperationMember] = operationID
	return params
}

func TestReceiptAnswersRepeatAfterReconnectWithoutSecondEffect(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	first := r.connectNative(ServeOptions{})
	threadID := first.startThread(r)
	operationID := rpc.NewOperationID()
	var started rpc.TurnStartResponse
	first.ok("turn/start", operationPrompt(threadID, "once", operationID), &started)
	first.awaitEvent("turn/completed", nil)
	_ = first.conn.Close()
	second := r.connectNative(ServeOptions{})
	var record rpc.OperationRecord
	second.ok("dalgona/operation/read", map[string]string{"operationId": operationID}, &record)
	var recorded rpc.TurnStartResponse
	if err := json.Unmarshal(record.Result, &recorded); err != nil || record.State != "succeeded" || record.Method != "turn/start" || recorded.Turn.ID != started.Turn.ID {
		t.Fatalf("operation record = %+v (%v)", record, err)
	}
	var repeated rpc.TurnStartResponse
	second.ok("turn/start", operationPrompt(threadID, "once", operationID), &repeated)
	if repeated.Turn.ID != started.Turn.ID || r.llm.requests() != 1 {
		t.Fatalf("repeat returned turn %s after %d model requests, want recorded %s and 1 request", repeated.Turn.ID, r.llm.requests(), started.Turn.ID)
	}
	requireOperationError(t, second.failsWith("turn/start", operationPrompt(threadID, "different", operationID), rpc.ErrorOperationConflict), operationID)
}

func requireOperationError(t *testing.T, failure *rpc.RPCError, operationID string) {
	t.Helper()
	if data, _ := rpc.ErrorDataOf(failure); data.OperationID != operationID {
		t.Fatalf("%s error names operation %q, want %q", failure.Message, data.OperationID, operationID)
	}
}

func TestUnknownOutcomeIsNeverDispatchedAgain(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	seed := r.connectNative(ServeOptions{})
	threadID := seed.startThread(r)
	operationID := rpc.NewOperationID()
	raw, err := json.Marshal(operationPrompt(threadID, "crashed", operationID))
	if err != nil {
		t.Fatal(err)
	}
	digest, err := paramDigest(raw)
	if err != nil {
		t.Fatal(err)
	}
	writeInterruptedReceipt(t, r.home, receipt{OperationID: operationID, Method: "turn/start", Digest: digest, State: stateRunning})
	c := r.connectNative(ServeOptions{})
	var record rpc.OperationRecord
	c.ok("dalgona/operation/read", map[string]string{"operationId": operationID}, &record)
	if record.State != stateUnknown {
		t.Fatalf("crashed operation state = %s, want unknown", record.State)
	}
	failure := c.failsWith("turn/start", operationPrompt(threadID, "crashed", operationID), rpc.ErrorUnknownOutcome)
	if data, _ := rpc.ErrorDataOf(failure); data.OperationID != operationID || failure.Code != -32017 {
		t.Fatalf("unknown outcome error = %d %+v", failure.Code, data)
	}
	if r.llm.requests() != 0 {
		t.Fatalf("an unknown operation dispatched %d model requests", r.llm.requests())
	}
}

func TestOperationIDNeedsNativeNegotiationAndFreshTimestamp(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	legacy := r.connect()
	threadID := startThread(t, r, legacy, nil)
	legacy.fails("turn/start", operationPrompt(threadID, "x", rpc.NewOperationID()), rpc.CodeInvalidParams, rpc.OperationMember+" requires the dalgonaNative capability")
	native := r.connectNative(ServeOptions{})
	stale := "01800000-0000-7000-8000-000000000000"
	requireOperationError(t, native.failsWith("turn/start", operationPrompt(threadID, "x", stale), rpc.ErrorInvalidParams), stale)
	if r.llm.requests() != 0 {
		t.Fatalf("rejected operations reached the model %d times", r.llm.requests())
	}
}

func TestDamagedReceiptGuardsItsOperationWithoutBlockingOthers(t *testing.T) {
	home := t.TempDir()
	damaged := rpc.NewOperationID()
	dir := filepath.Join(home, receiptDirName)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, damaged+receiptSuffix), []byte(`{"operationId":`), 0o600); err != nil {
		t.Fatal(err)
	}
	store, err := receiptsFor(home, func(rec receipt) (receipt, bool) { return rec, false })
	if err != nil {
		t.Fatalf("one damaged receipt blocked the store: %v", err)
	}
	now := time.Now()
	_, _, err = store.reserve(operation{method: "turn/start", id: damaged, digest: "d"}, now)
	if data, ok := rpc.ErrorDataOf(err); !ok || data.Kind != rpc.ErrorUnknownOutcome {
		t.Fatalf("damaged operation repeat error = %v", err)
	}
	if _, replay, err := store.reserve(operation{method: "turn/start", id: rpc.NewOperationID(), digest: "d"}, now); err != nil || replay {
		t.Fatalf("fresh operation after damage: replay=%v err=%v", replay, err)
	}
}
