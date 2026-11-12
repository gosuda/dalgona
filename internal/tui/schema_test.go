package tui

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"path/filepath"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/santhosh-tekuri/jsonschema/v6"

	"github.com/gosuda/dalgona/internal/rpc"
)

const schemaThread = `{"id":"t1","sessionId":"s1","name":"demo","preview":"fix the test","modelProvider":"openai",` +
	`"createdAt":1,"updatedAt":2,"cwd":"/work","ephemeral":false,"cliVersion":"0.1.0","source":"appServer",` +
	`"projectId":null,"status":{"type":"idle"},"turns":[` +
	`{"id":"turn-0","status":"completed","items":[` +
	`{"type":"userMessage","id":"u0","clientId":null,"content":[{"type":"text","text":"earlier prompt","text_elements":[]}]},` +
	`{"type":"commandExecution","id":"c0","command":"go vet ./...","commandActions":[],"cwd":"/work","status":"completed","aggregatedOutput":"vet clean","exitCode":0}` +
	`]}]}`

func schemaValue(t *testing.T, name string, data string) jsontext.Value {
	t.Helper()
	path := filepath.Join("..", "rpc", "testdata", "codex-schema", "v2", name+".json")
	compiled, err := jsonschema.NewCompiler().Compile(path)
	if err != nil {
		t.Fatalf("compile %s: %v", name, err)
	}
	var value any
	if err := json.Unmarshal([]byte(data), &value); err != nil {
		t.Fatalf("fixture %s is not JSON: %v", name, err)
	}
	if err := compiled.Validate(value); err != nil {
		t.Fatalf("fixture %s does not match the codex schema: %v", name, err)
	}
	return jsontext.Value(data)
}

func schemaNotification(t *testing.T, m model, method, schema, data string) model {
	t.Helper()
	m, _ = drive(t, m, notificationMsg{method: method, params: schemaValue(t, schema, data)})
	if strings.Contains(m.notice, "protocol error") {
		t.Fatalf("%s: %s", method, m.notice)
	}
	return m
}

func schemaStartedModel(t *testing.T) model {
	t.Helper()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 120
	m = schemaNotification(t, m, "thread/started", "ThreadStartedNotification", `{"thread":`+schemaThread+`}`)
	if m.thread.id != "t1" || m.thread.title != "demo" || m.thread.state != stateIdle {
		t.Fatalf("thread/started: id %q title %q state %q", m.thread.id, m.thread.title, m.thread.state)
	}
	return m
}

func TestModelAppliesSchemaExactThreadNotifications(t *testing.T) {
	t.Parallel()
	m := schemaStartedModel(t)
	m = schemaNotification(t, m, "thread/status/changed", "ThreadStatusChangedNotification",
		`{"threadId":"t1","status":{"type":"active","activeFlags":["waitingOnApproval"]}}`)
	if m.thread.state != stateAwaitingApproval {
		t.Fatalf("waitingOnApproval state = %q", m.thread.state)
	}
	m = schemaNotification(t, m, "thread/status/changed", "ThreadStatusChangedNotification",
		`{"threadId":"t1","status":{"type":"active","activeFlags":[]}}`)
	if m.thread.state != stateRunning {
		t.Fatalf("active state = %q", m.thread.state)
	}
	m = schemaNotification(t, m, "thread/name/updated", "ThreadNameUpdatedNotification", `{"threadId":"t1","threadName":"renamed"}`)
	if m.thread.title != "renamed" {
		t.Fatalf("thread name = %q", m.thread.title)
	}
}

func TestModelAppliesSchemaExactTurnNotifications(t *testing.T) {
	t.Parallel()
	m := schemaStartedModel(t)
	m.notice = "error: earlier failure; retry the turn, /resume t1, or /new"
	m = schemaNotification(t, m, "turn/started", "TurnStartedNotification", `{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`)
	if m.notice != "" || m.thread.turnID != "turn-1" {
		t.Fatalf("turn/started kept notice %q, turn %q", m.notice, m.thread.turnID)
	}
	m = schemaNotification(t, m, "turn/plan/updated", "TurnPlanUpdatedNotification",
		`{"threadId":"t1","turnId":"turn-1","plan":[{"step":"read the test","status":"completed"},{"step":"fix the decoder","status":"inProgress"}]}`)
	assertContains(t, m.notice, "[done] read the test", "[in progress] fix the decoder")
	m = schemaNotification(t, m, "item/started", "ItemStartedNotification",
		`{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"commandExecution","id":"c1","command":"go test ./...","commandActions":[],"cwd":"/work","status":"inProgress","aggregatedOutput":null}}`)
	m = schemaNotification(t, m, "item/commandExecution/outputDelta", "CommandExecutionOutputDeltaNotification",
		`{"threadId":"t1","turnId":"turn-1","itemId":"c1","delta":"ok  internal/tui"}`)
	m, _ = drive(t, m, press(tea.KeyLeft, 0, ""))
	assertContains(t, m.View().Content, "Running go test ./...", "ok  internal/tui")
	m, text := schemaSettled(t, m, `{"threadId":"t1","turnId":"turn-1","completedAtMs":2,"item":{"type":"commandExecution","id":"c1",`+
		`"command":"go test ./...","commandActions":[],"cwd":"/work","status":"completed","aggregatedOutput":"ok  internal/tui\nPASS","exitCode":0}}`)
	assertContains(t, text, "$ go test ./...", "PASS")
	m, text = schemaSettled(t, m, `{"threadId":"t1","turnId":"turn-1","completedAtMs":3,"item":{"type":"userMessage","id":"u1",`+
		`"clientId":null,"content":[{"type":"text","text":"steer toward the decoder","text_elements":[]}]}}`)
	assertContains(t, text, "you", "steer toward the decoder")
	_, text = schemaSettled(t, m, `{"threadId":"t1","turnId":"turn-1","completedAtMs":4,"item":{"content":[{"type":"text",`+
		`"text":"content before type","text_elements":[]}],"clientId":null,"id":"u2","type":"userMessage"}}`)
	assertContains(t, text, "you", "content before type")
}

func TestModelKeepsStreamedTextOnCompletion(t *testing.T) {
	t.Parallel()
	m := schemaStartedModel(t)
	m = schemaNotification(t, m, "item/started", "ItemStartedNotification",
		`{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"agentMessage","id":"a1","text":""}}`)
	m = schemaNotification(t, m, "item/agentMessage/delta", "AgentMessageDeltaNotification",
		`{"delta":"caf\u00e9 \"quoted\" ","itemId":"a1","turnId":"turn-1","threadId":"t1"}`)
	m = schemaNotification(t, m, "item/agentMessage/delta", "AgentMessageDeltaNotification",
		`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":"answer"}`)
	assertContains(t, m.View().Content, `café "quoted" answer`)
	_, text := schemaSettled(t, m, `{"threadId":"t1","turnId":"turn-1","completedAtMs":2,"item":{"type":"agentMessage","id":"a1","text":""}}`)
	assertContains(t, text, `café "quoted" answer`)
}

func schemaSettled(t *testing.T, m model, data string) (model, string) {
	t.Helper()
	m, command := drive(t, m, notificationMsg{method: "item/completed", params: schemaValue(t, "ItemCompletedNotification", data)})
	if command == nil || strings.Contains(m.notice, "protocol error") {
		t.Fatalf("item/completed: command %v, notice %q", command, m.notice)
	}
	if len(m.live.items) != 0 {
		t.Fatalf("completed item is still live: %#v", m.live.items)
	}
	return m, stripANSI(lastEntry(t, m))
}

func TestModelAppliesSchemaExactErrors(t *testing.T) {
	t.Parallel()
	m := schemaStartedModel(t)
	m = schemaNotification(t, m, "turn/started", "TurnStartedNotification", `{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`)
	m = schemaNotification(t, m, "error", "ErrorNotification", `{"threadId":"t1","turnId":"turn-1","willRetry":true,"error":{"message":"stream reset"}}`)
	if m.thread.state != stateRunning || !strings.Contains(m.notice, "retrying the turn") {
		t.Fatalf("retrying error: state %q notice %q", m.thread.state, m.notice)
	}
	m = schemaNotification(t, m, "error", "ErrorNotification", `{"threadId":"t1","turnId":"turn-1","willRetry":false,"error":{"message":"stream reset"}}`)
	if m.thread.state != stateFailed || !strings.Contains(m.notice, "/resume t1") {
		t.Fatalf("final error: state %q notice %q", m.thread.state, m.notice)
	}
	m = schemaNotification(t, m, "turn/completed", "TurnCompletedNotification",
		`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"failed","error":{"message":"stream reset"}}}`)
	if m.thread.state != stateFailed || m.thread.turnID != "" {
		t.Fatalf("failed turn: state %q turn %q", m.thread.state, m.thread.turnID)
	}
}

func TestModelRendersSchemaExactResponses(t *testing.T) {
	t.Parallel()
	var read rpc.ThreadReadResponse
	if err := json.Unmarshal(schemaValue(t, "ThreadReadResponse", `{"thread":`+schemaThread+`}`), &read); err != nil {
		t.Fatalf("decode thread/read: %v", err)
	}
	m := newModel(nil, Options{}, nil)
	m.screen.width = 120
	m, command := drive(t, m, resultMsg{op: "resumed", value: &read})
	if command == nil || m.thread.id != "t1" || m.thread.title != "demo" || m.thread.state != stateIdle {
		t.Fatalf("resumed: command %v id %q title %q state %q", command, m.thread.id, m.thread.title, m.thread.state)
	}
	text := stripANSI(strings.Join([]string{
		m.renderSettledItem(read.Thread.Turns[0].Items[0]),
		m.renderSettledItem(read.Thread.Turns[0].Items[1]),
	}, "\n"))
	assertContains(t, text, "earlier prompt", "$ go vet ./...", "vet clean")

	var list rpc.ThreadListResponse
	if err := json.Unmarshal(schemaValue(t, "ThreadListResponse", `{"data":[`+schemaThread+`],"nextCursor":null}`), &list); err != nil {
		t.Fatalf("decode thread/list: %v", err)
	}
	m, _ = drive(t, m, resultMsg{op: "threads", value: &list})
	assertContains(t, m.View().Content, "resume a thread", "demo (active) — idle")
}
