package tui

import (
	"fmt"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const storedPatchChange = `[{"path":"a.go","kind":{"type":"update"},"diff":"--- a/a.go\n+++ b/a.go\n@@ -1 +1 @@\n-old\n+new\n","additions":1,"deletions":1}]`

func patchUpdate(t *testing.T, m model, threadID, itemID string) model {
	t.Helper()
	params := fmt.Sprintf(`{"threadId":%q,"turnId":"turn-1","itemId":%q,"changes":%s}`, threadID, itemID, storedPatchChange)
	return rawUpdate(t, m, notificationMsg{method: "item/fileChange/patchUpdated", params: []byte(params)})
}

func fileChangeCompleted(t *testing.T, m model, threadID, itemID string) model {
	t.Helper()
	params := fmt.Sprintf(`{"threadId":%q,"turnId":"turn-1","completedAtMs":2,"item":{"type":"fileChange","id":%q,"status":"completed","changes":%s}}`, threadID, itemID, storedPatchChange)
	return rawUpdate(t, m, notificationMsg{method: "item/completed", params: []byte(params)})
}

func TestPatchStoreStaysBoundedOverAutoApprovedTurn(t *testing.T) {
	t.Parallel()
	const items = 500
	m := newTestModel(nil, Options{})
	m.thread.id = "t1"
	for i := range items {
		m = patchUpdate(t, m, "t1", fmt.Sprintf("f%d", i))
	}
	if got := len(m.live.patches); got != items {
		t.Fatalf("patches while the items run = %d, want %d", got, items)
	}
	for i := range items {
		m = fileChangeCompleted(t, m, "t1", fmt.Sprintf("f%d", i))
	}
	if got := len(m.live.patches); got != 0 {
		t.Fatalf("patches after every item/completed = %d, want 0", got)
	}
}

func TestPatchStoreDropsChildThreadPatchWhenItsItemSettles(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.thread.id = "t1"
	m = patchUpdate(t, m, "child", "f1")
	m = fileChangeCompleted(t, m, "child", "f1")
	if got := len(m.live.patches); got != 0 {
		t.Fatalf("patches after the child item settled = %d, want 0", got)
	}
}

func TestPatchStoreKeepsPatchOfOpenApprovalUntilResolved(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.thread.id = "t1"
	m = patchUpdate(t, m, "t1", "fc1")
	m = patchUpdate(t, m, "t1", "fc2")
	m = rawUpdate(t, m, fileApprovalRequest(make(chan requestAnswer, 1)))
	m = fileChangeCompleted(t, m, "t1", "fc1")
	m = fileChangeCompleted(t, m, "t1", "fc2")
	if _, ok := m.live.patches[patchKey{"t1", "fc1"}]; !ok {
		t.Fatal("item/completed dropped the patch that an open approval shows")
	}
	if _, ok := m.live.patches[patchKey{"t1", "fc2"}]; ok {
		t.Fatal("item/completed kept the patch of an item without an approval")
	}
	m = rawUpdate(t, m, notificationMsg{method: "serverRequest/resolved", params: []byte(`{"threadId":"t1","requestId":"` + m.approvals[0].requestID + `","resolution":"answered"}`)})
	if got := len(m.live.patches); got != 0 {
		t.Fatalf("patches after the approval resolved = %d, want 0", got)
	}
}

func TestPatchStoreDropsUnclaimedPatchesOnThreadSwitch(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.thread.id = "t1"
	m = patchUpdate(t, m, "t1", "f1")
	m = patchUpdate(t, m, "t1", "f2")
	m = patchUpdate(t, m, "child", "fc1")
	m = rawUpdate(t, m, serverRequestMsg{
		method: "item/fileChange/requestApproval",
		params: []byte(`{"threadId":"child","turnId":"turn-2","itemId":"fc1","startedAtMs":1,"reason":"edit a.go"}`),
		answer: make(chan requestAnswer, 1),
	})
	m = rawUpdate(t, m, resultMsg{op: "new", value: &rpc.ThreadStartResponse{Thread: rpc.Thread{ID: "t2"}}})
	if m.thread.id != "t2" {
		t.Fatalf("thread = %q, want t2", m.thread.id)
	}
	if got := len(m.live.patches); got != 1 {
		t.Fatalf("patches after /new = %d, want only the open approval's", got)
	}
	if _, ok := m.live.patches[patchKey{"child", "fc1"}]; !ok {
		t.Fatal("thread switch dropped the patch that an open approval shows")
	}
}
