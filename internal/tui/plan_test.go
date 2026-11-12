package tui

import (
	"slices"
	"strconv"
	"strings"
	"testing"
)

func planUpdate(t *testing.T, m model, plan string) model {
	t.Helper()
	params := []byte(`{"threadId":"t1","turnId":"turn-1","plan":` + plan + `}`)
	return rawUpdate(t, m, notificationMsg{method: "turn/plan/updated", params: params})
}

func planKeys(m model) []string {
	var keys []string
	for _, key := range entryKeys(m) {
		if strings.HasPrefix(key, "plan:") {
			keys = append(keys, key)
		}
	}
	return keys
}

func subagentItem(id, status, result string) string {
	return `{"type":"dynamicToolCall","id":"` + id + `","tool":"task","arguments":{"prompt":"count the files"},"status":"` + status + `","contentItems":[{"type":"inputText","text":` + strconv.Quote(result) + `}]}`
}

func childSpawned(t *testing.T, m model, child string) model {
	t.Helper()
	params := []byte(`{"threadId":"t1","childId":"` + child + `","cwd":"/work"}`)
	return rawUpdate(t, m, notificationMsg{method: "dalgona/thread/childSpawned", params: params})
}

func dockText(m model) string {
	return stripANSI(strings.Join(m.dockLines(), "\n"))
}

func wrappedText(text string) string {
	return strings.Join(strings.Fields(text), " ")
}

func TestPlanRowPerChange(t *testing.T) {
	t.Parallel()
	m := notifyTurn(t, rowModel(t, 40), "turn/started", "inProgress")
	m = planUpdate(t, m, `[{"step":"read the test","status":"inProgress"}]`)
	m = planUpdate(t, m, `[{"step":"read the test","status":"completed"}]`)
	if got, want := planKeys(m), []string{"plan:turn-1:0", "plan:turn-1:1"}; !slices.Equal(got, want) {
		t.Fatalf("plan keys = %v, want %v", got, want)
	}
	if text := entryText(t, m, "plan:turn-1:1"); text != "Plan\nDone read the test" {
		t.Fatalf("plan row = %q, want %q", text, "Plan\nDone read the test")
	}
	assertFrame(t, m.dockLines(), 40, 24)
}

func TestPlanRowShowsFiveStepsThenMore(t *testing.T) {
	t.Parallel()
	m := notifyTurn(t, rowModel(t, 80), "turn/started", "inProgress")
	m = planUpdate(t, m, `[{"step":"one","status":"completed"},{"step":"two","status":"completed"},{"step":"three","status":"inProgress"},{"step":"four","status":"pending"},{"step":"five","status":"pending"},{"step":"six","status":"pending"},{"step":"seven","status":"pending"}]`)
	if text := entryText(t, m, "plan:turn-1:0"); text != "Plan\nDone one\nDone two\nWorking three\nPending four\nPending five\n+2 more" {
		t.Fatalf("plan row = %q", text)
	}
}

func TestPlanFieldClearsOnTurnCompleted(t *testing.T) {
	t.Parallel()
	m := notifyTurn(t, rowModel(t, 80), "turn/started", "inProgress")
	m = planUpdate(t, m, `[{"step":"read the test","status":"completed"},{"step":"fix it","status":"pending"}]`)
	assertContains(t, stripANSI(m.liveRow()), "1/2 plan")
	m = notifyTurn(t, m, "turn/completed", "completed")
	if strings.Contains(stripANSI(m.liveRow()), "plan") {
		t.Fatalf("live row keeps the plan field: %q", m.liveRow())
	}
}

func TestSubagentRowAtEnd(t *testing.T) {
	t.Parallel()
	m := notifyTurn(t, rowModel(t, 40), "turn/started", "inProgress")
	before := len(m.out.entries)
	m = notifyItem(t, m, "item/started", subagentItem("task-1", "inProgress", ""))
	assertContains(t, dockText(m), "1 run")
	if len(m.out.entries) != before {
		t.Fatalf("a running subagent committed %d rows", len(m.out.entries)-before)
	}
	m = childSpawned(t, m, "child-1")
	m = notifyItem(t, m, "item/completed", subagentItem("task-1", "completed", `{"data":"5 files"}`))
	text := wrappedText(entryText(t, m, "item:t1:task-1"))
	assertContains(t, text, "Subagent", "child child-1 · /resume child-1", "5 files")
	assertFrame(t, m.dockLines(), 40, 24)
}

func TestSubagentErrorRowShowsError(t *testing.T) {
	t.Parallel()
	m := notifyTurn(t, rowModel(t, 80), "turn/started", "inProgress")
	m = notifyItem(t, m, "item/started", subagentItem("task-2", "inProgress", ""))
	m = notifyItem(t, m, "item/completed", subagentItem("task-2", "completed", `{"error":"no model"}`))
	assertContains(t, entryText(t, m, "item:t1:task-2"), "error: no model")
}

func TestChildAttachesToRunningTaskCall(t *testing.T) {
	t.Parallel()
	m := notifyTurn(t, rowModel(t, 80), "turn/started", "inProgress")
	m = notifyItem(t, m, "item/started", subagentItem("task-1", "inProgress", ""))
	m = notifyItem(t, m, "item/started", subagentItem("task-2", "inProgress", ""))
	m = notifyItem(t, m, "item/completed", subagentItem("task-1", "completed", `{"data":"first"}`))
	m = childSpawned(t, m, "child-2")
	m = notifyItem(t, m, "item/completed", subagentItem("task-2", "completed", `{"data":"second"}`))
	assertContains(t, wrappedText(entryText(t, m, "item:t1:task-2")), "child child-2 · /resume child-2")
	if text := wrappedText(entryText(t, m, "item:t1:task-1")); strings.Contains(text, "child-2") {
		t.Fatalf("finished task-1 row lists child-2: %q", text)
	}
}

func TestChildSkipsFinishedTaskItem(t *testing.T) {
	t.Parallel()
	m := notifyTurn(t, rowModel(t, 80), "turn/started", "inProgress")
	m = notifyItem(t, m, "item/started", subagentItem("task-1", "inProgress", ""))
	m = notifyItem(t, m, "item/started", subagentItem("task-2", "completed", ""))
	m = childSpawned(t, m, "child-1")
	for _, item := range m.live.items {
		if item.ID == "task-2" && len(item.children) != 0 {
			t.Fatalf("finished task-2 received children %v", item.children)
		}
	}
	if children := m.live.items[0].children; !slices.Equal(children, []string{"child-1"}) {
		t.Fatalf("running task-1 children = %v, want [child-1]", children)
	}
}

func TestPlanKeyUsesNotificationTurn(t *testing.T) {
	t.Parallel()
	m := notifyTurn(t, rowModel(t, 80), "turn/started", "inProgress")
	params := []byte(`{"threadId":"t1","turnId":"turn-2","plan":[{"step":"read","status":"inProgress"}]}`)
	for range 2 {
		m = rawUpdate(t, m, notificationMsg{method: "turn/plan/updated", params: params})
	}
	if got, want := planKeys(m), []string{"plan:turn-2:0", "plan:turn-2:1"}; !slices.Equal(got, want) {
		t.Fatalf("plan keys = %v, want %v", got, want)
	}
}
