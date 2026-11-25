package rpcserver

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func TestWorkspaceSearchRejectsListingCursor(t *testing.T) {
	s, h := &server{}, &workspaceHandle{}
	id, err := s.workspace.remember(&workspaceCursor{handle: h, path: "docs", offset: 1})
	if err != nil {
		t.Fatal(err)
	}
	_, err = s.searchResults(workspaceUse{ctx: t.Context(), handle: h}, workspaceSearchParams{Query: "docs", Cursor: id})
	refusedWith(t, err, rpc.ErrorExpired)
}

func TestWorkspaceSearchCursorRejectsChangedNestedPaths(t *testing.T) {
	eachWorkspaceRunner(t, func(t *testing.T, r *rig) {
		dir := filepath.Join(r.cwd, "docs")
		if err := os.Mkdir(dir, 0o700); err != nil {
			t.Fatal(err)
		}
		for _, name := range []string{"alpha", "alpine", "alps"} {
			if err := os.WriteFile(filepath.Join(dir, name), nil, 0o600); err != nil {
				t.Fatal(err)
			}
		}
		w := openWorkspaceWindow(t, r)
		opened := w.open(startWorkspaceThread(t, r, r.cwd), "read")
		const method = "dalgona/workspace/search"
		params := map[string]any{"workspaceId": opened.WorkspaceID, "query": "alp", "limit": 1}
		var page workspaceSearchPage
		w.must(method, params, &page)
		if page.NextCursor == "" {
			t.Fatal("expected a continuation for three matches")
		}
		params["cursor"] = page.NextCursor
		w.must(method, params, &page)
		params["cursor"] = page.NextCursor
		if page.NextCursor == "" {
			t.Fatal("unchanged workspace lost the second continuation")
		}
		if err := os.Rename(filepath.Join(dir, "alps"), filepath.Join(dir, "beta")); err != nil {
			t.Fatal(err)
		}
		refusedWith(t, w.call(method, params, nil), rpc.ErrorStaleRevision)
	})
}

func TestWorkspaceExecInvalidTextDoesNotReserveOperation(t *testing.T) {
	r := newRig(t, "")
	w := openWorkspaceWindow(t, r)
	opened := w.open(startWorkspaceThread(t, r, r.cwd), "terminal")
	id := rpc.NewOperationID()
	params := map[string]any{"workspaceId": opened.WorkspaceID, "text": "invalid\x00", "operationId": id}
	err := w.call("dalgona/workspace/exec", params, nil)
	coded, ok := errors.AsType[*rpc.RPCError](err)
	if !ok || coded.Code != rpc.CodeInvalidParams || coded.Message != "invalid text: text: disallowed_control" {
		t.Fatalf("invalid shell text returned %v", err)
	}
	refusedWith(t, w.call("dalgona/operation/read", map[string]string{"operationId": id}, nil), rpc.ErrorNotFound)
}

func TestWorkspaceUnsubscribeWithdrawsPendingPermission(t *testing.T) {
	r := newRig(t, "")
	threadID := startWorkspaceThread(t, r, r.cwd)
	w := openWorkspaceWindow(t, r)
	if err := w.conn.Call(t.Context(), "thread/resume", map[string]string{"threadId": threadID}, nil); err != nil {
		t.Fatal(err)
	}
	result := make(chan error, 1)
	go func() {
		params := map[string]any{"threadId": threadID, "root": "cwd", "rights": []string{"read"}, "operationId": rpc.NewOperationID()}
		result <- w.conn.Call(t.Context(), "dalgona/workspace/open", params, nil)
	}()
	select {
	case <-w.asked:
	case <-time.After(waitLimit):
		t.Fatal("no workspace permission request")
	}
	if err := w.conn.Call(t.Context(), "thread/unsubscribe", map[string]string{"threadId": threadID}, nil); err != nil {
		t.Fatal(err)
	}
	w.decisions <- workspaceAllowOnce
	select {
	case err := <-result:
		refusedWith(t, err, rpc.ErrorPermissionDenied)
	case <-time.After(waitLimit):
		t.Fatal("withdrawn workspace operation did not settle")
	}
}

func TestFullSubscriptionsDoNotCreateThreads(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	var threadID string
	for range 8 {
		threadID = c.startThread(r)
	}
	c.ok("turn/start", prompt(threadID, "turn to fork"), nil)
	c.awaitEvent("turn/completed", nil)
	cases := []struct {
		method string
		params map[string]any
	}{
		{"thread/start", map[string]any{"cwd": r.cwd}},
		{"thread/fork", map[string]any{"threadId": threadID}},
	}
	for _, tc := range cases {
		t.Run(tc.method, func(t *testing.T) {
			tc.params[rpc.OperationMember] = rpc.NewOperationID()
			before := len(r.host.AllThreads())
			_ = c.failsWith(tc.method, tc.params, rpc.ErrorLimitExceeded)
			if after := len(r.host.AllThreads()); after != before {
				t.Fatalf("%s refused but durable thread count changed from %d to %d", tc.method, before, after)
			}
		})
	}
}

func TestExistingWatchWaitsForJournalReplay(t *testing.T) {
	r := newRig(t, "")
	id := startWorkspaceThread(t, r, r.cwd)
	thread, err := r.host.Resume(t.Context(), id)
	if err != nil {
		t.Fatal(err)
	}
	s := &server{ctx: t.Context(), host: r.host, watches: map[string]*watch{}, log: slog.Default()}
	entered, resume, finished := make(chan struct{}), make(chan struct{}), make(chan struct{})
	go func() {
		defer close(finished)
		_, _, _ = s.watch(t.Context(), thread, func(tr *tracker, records []event.Seqd) []rpc.Turn {
			close(entered)
			<-resume
			return tr.replayLive(records)
		})
	}()
	<-entered
	t.Cleanup(func() {
		close(resume)
		<-finished
		s.unsubscribe(id)
	})
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, _, err := s.watch(ctx, thread, (*tracker).replayLive); !errors.Is(err, context.Canceled) {
		t.Fatalf("existing unready watch returned %v, want canceled wait", err)
	}
}

func TestClosedHistoryKeepsAdmittedSpillRead(t *testing.T) {
	acc := &accumulator{}
	const tail = "tail from spill"
	acc.append([]byte(strings.Repeat("a", 256<<10) + tail))
	frozen, err := acc.freeze()
	if err != nil {
		t.Fatal(err)
	}
	acc.finish()
	table := newHistoryTable()
	snap := &historySnapshot{id: "history", expires: time.Now().Add(time.Minute), active: []frozenItem{{content: frozen}}}
	if publishErr := table.publish(snap, "", time.Now()); publishErr != nil {
		t.Fatal(publishErr)
	}
	admitted, err := table.acquire(snap.id, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	defer table.done(admitted)
	table.close(snap.id)
	_, err = table.acquire(snap.id, time.Now())
	refusedWith(t, err, rpc.ErrorExpired)
	got, err := admitted.active[0].content.read(256<<10, int64(len(tail)))
	if err != nil || string(got) != tail {
		t.Fatalf("admitted read after close = %q, %v; want %q", got, err, tail)
	}
}

func TestReceiptAdmissionAccountsForFinalOutcome(t *testing.T) {
	r := pagingServer(t)
	store, err := r.receipts()
	if err != nil {
		t.Fatal(err)
	}
	store.bytes = (32 << 20) - (1 << 20)
	op := operation{method: "test/large", id: rpc.NewOperationID(), digest: "digest"}
	called := false
	_, err = r.runOperation(t.Context(), op, func(context.Context) (jsontext.Value, error) {
		called = true
		return jsontext.Value(`"` + strings.Repeat("x", (2<<20)-2) + `"`), nil
	})
	refusedWith(t, err, rpc.ErrorLimitExceeded)
	if called {
		t.Fatal("receipt capacity refusal dispatched the effect")
	}
}

func TestReceiptFinalizationFailureReturnsUnknownOperation(t *testing.T) {
	r := pagingServer(t)
	store, err := r.receipts()
	if err != nil {
		t.Fatal(err)
	}
	op := operation{method: "test/finalize", id: rpc.NewOperationID(), digest: "digest"}
	var sabotage error
	_, err = r.runOperation(t.Context(), op, func(context.Context) (jsontext.Value, error) {
		path := store.path(op.id)
		sabotage = errors.Join(os.Remove(path), os.Mkdir(path, 0o700))
		return emptyObject(), nil
	})
	if sabotage != nil {
		t.Fatal(sabotage)
	}
	if data := refusedWith(t, err, rpc.ErrorUnknownOutcome); data.OperationID != op.id {
		t.Fatalf("unknown outcome named %q, want %q", data.OperationID, op.id)
	}
}

func TestReceiptReservationAndReplacementSurviveReopen(t *testing.T) {
	home := t.TempDir()
	store, err := receiptsFor(home, func(rec receipt) (receipt, bool) { return rec, false })
	if err != nil {
		t.Fatal(err)
	}
	op := operation{method: "test/durable", id: rpc.NewOperationID(), digest: "digest"}
	rec, _, err := store.reserve(op, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	if reserved := readDiskReceipt(t, store.path(op.id)); reserved.State != stateReserved {
		t.Fatalf("reservation state = %s, want reserved", reserved.State)
	}
	if finishErr := store.finish(rec, jsontext.Value(`{"ok":true}`), nil, false); finishErr != nil {
		t.Fatal(finishErr)
	}
	disk := readDiskReceipt(t, store.path(op.id))
	if disk.State != stateSucceeded || string(disk.Result) != `{"ok":true}` {
		t.Fatalf("replacement receipt = %s %s", disk.State, disk.Result)
	}
}

func readDiskReceipt(t *testing.T, path string) receipt {
	t.Helper()
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var rec receipt
	if decodeErr := json.Unmarshal(raw, &rec); decodeErr != nil {
		t.Fatal(decodeErr)
	}
	return rec
}

func TestNativeMCPStatusUsesExactStaleKind(t *testing.T) {
	r := newRig(t, "")
	r.cfg.MCPInventory = func(context.Context, string) (agent.MCPInventory, error) {
		return agent.MCPInventory{Version: "current"}, nil
	}
	r.openHost()
	nativeConfig := func(context.Context, jsontext.Value) (jsontext.Value, error) { return emptyObject(), nil }
	c := r.connectNative(ServeOptions{Extensions: map[string]Extension{
		"dalgona/mcp/config/read": nativeConfig, "dalgona/mcp/config/write": nativeConfig,
	}})
	threadID := c.startThread(r)
	_ = c.failsWith("mcpServerStatus/list", map[string]any{"threadId": threadID, "configVersion": "obsolete"}, rpc.ErrorStaleRevision)
}

func TestLiveEventSpillRespectsDaemonCapacity(t *testing.T) {
	native := &nativeSession{streamID: "stream", contents: newLiveContents()}
	for _, thread := range []string{"held-a", "held-b", "held-c", "held-d"} {
		if !agent.ReserveActiveSpill(thread, 256<<20) {
			t.Fatal("could not fill the test spill budget")
		}
		t.Cleanup(func() { agent.ReleaseActiveSpill(thread, 256<<20) })
	}
	_, err := native.spillEvent("thread", 1, jsontext.Value(`{"diff":"spilled content"}`))
	refusedWith(t, err, rpc.ErrorLimitExceeded)
}

func TestLiveEventSpillRemainsChargedWhileAdmittedReadRuns(t *testing.T) {
	native := &nativeSession{streamID: "stream", contents: newLiveContents()}
	defer native.contents.close()
	const threadBytes = 256 << 20
	ref, err := native.spillEvent("thread", 1, jsontext.Value(`{"status":"finished"}`))
	if err != nil {
		t.Fatal(err)
	}
	admitted, err := native.contents.lookup(ref.ContentID, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	native.contents.forgetThread("thread")
	if agent.ReserveActiveSpill("thread", threadBytes) {
		agent.ReleaseActiveSpill("thread", threadBytes)
		t.Error("retirement refunded an admitted spill")
	}
	got, err := admitted.content.read(0, admitted.total)
	admitted.releaseContent()
	if err != nil || !bytes.Contains(got, []byte(`"status":"finished"`)) {
		t.Fatalf("reading retired spill: %q (%v)", got, err)
	}
	if !agent.ReserveActiveSpill("thread", threadBytes) {
		t.Fatal("completed read did not refund its spill")
	}
	agent.ReleaseActiveSpill("thread", threadBytes)
}
