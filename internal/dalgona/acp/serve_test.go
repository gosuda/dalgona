package acp

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"io"
	"reflect"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func TestInitializeNegotiatesProtocol(t *testing.T) {
	f := newFixture(t)
	id := f.ed.request("initialize", map[string]any{"protocolVersion": 1, "clientCapabilities": map[string]any{}})
	var got initializeResult
	f.ed.result(id, &got)
	if want := newInitializeResult(testVersion); !reflect.DeepEqual(got, want) {
		t.Fatalf("initialize = %+v, want %+v", got, want)
	}
	call := f.d.await("initialize")
	params := paramsOf[rpc.InitializeParams](t, call)
	if params.ClientInfo.Name != clientName || !params.Capabilities.RequiredHumanApproval {
		t.Fatalf("daemon initialize params = %+v", params)
	}
	if f.dials.Load() != 1 {
		t.Fatalf("dials = %d, want 1", f.dials.Load())
	}
}

func TestInitializeMissingProtocolVersion(t *testing.T) {
	f := newFixture(t)
	id := jsontext.Value("1")
	f.ed.raw(`{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientCapabilities":{}}}`)
	err := f.ed.failure(id)
	if err.Code != codeInvalidParams {
		t.Fatalf("code = %d, want %d", err.Code, codeInvalidParams)
	}
	if f.d.count("initialize") != 0 {
		t.Fatal("a rejected initialize must not reach the daemon")
	}
}

func TestInitializeDialFailure(t *testing.T) {
	f := newFixture(t)
	_ = f.d.conn.Close()
	id := f.ed.request("initialize", map[string]any{"protocolVersion": 1})
	err := f.ed.failure(id)
	if err.Code != codeInternal {
		t.Fatalf("code = %d, want %d", err.Code, codeInternal)
	}
}

func TestRequestBeforeInitialize(t *testing.T) {
	f := newFixture(t)
	id := f.ed.request("session/new", map[string]any{"cwd": "/work", "mcpServers": []any{}})
	err := f.ed.failure(id)
	if err.Code != codeInvalidRequest {
		t.Fatalf("code = %d, want %d", err.Code, codeInvalidRequest)
	}
	id = f.ed.request("session/prompt", map[string]any{"sessionId": "s", "prompt": []any{promptText("hi")}})
	err = f.ed.failure(id)
	if err.Code != codeInvalidRequest || err.Message != "initialize has not run" {
		t.Fatalf("prompt before initialize: %+v", err)
	}
	id = f.ed.request("session/cancel", map[string]any{"sessionId": "s"})
	err = f.ed.failure(id)
	if err.Code != codeInvalidRequest || err.Message != "initialize has not run" {
		t.Fatalf("cancel before initialize: %+v", err)
	}
	id = f.ed.request("session/set_config_option", map[string]any{"sessionId": "s", "configId": "model", "value": "m"})
	err = f.ed.failure(id)
	if err.Code != codeInvalidRequest || err.Message != "initialize has not run" {
		t.Fatalf("set_config_option before initialize: %+v", err)
	}
}

func TestUnknownMethod(t *testing.T) {
	f := newFixture(t)
	id := f.ed.request("session/mode", map[string]any{"sessionId": "s"})
	err := f.ed.failure(id)
	if err.Code != codeMethodNotFound {
		t.Fatalf("code = %d, want %d", err.Code, codeMethodNotFound)
	}
}

func TestMalformedLineRejected(t *testing.T) {
	f := newFixture(t)
	f.ed.raw("{not json")
	m := f.ed.next()
	if m.Method != "" || m.Error == nil {
		t.Fatalf("expected an error response, got %+v", m)
	}
	if m.Error.Code != codeParse && m.Error.Code != codeInvalidRequest {
		t.Fatalf("error code = %d", m.Error.Code)
	}
}

func TestNonRequestRejected(t *testing.T) {
	f := newFixture(t)
	f.ed.raw("null")
	f.ed.raw("{}")
	m := f.ed.next()
	if m.Method != "" || m.Error == nil {
		t.Fatalf("expected an error response, got %+v", m)
	}
	if m.Error.Code != codeInvalidRequest {
		t.Fatalf("error code = %d, want %d", m.Error.Code, codeInvalidRequest)
	}
	m = f.ed.next()
	if m.Method != "" || m.Error == nil {
		t.Fatalf("expected an error response, got %+v", m)
	}
	if m.Error.Code != codeInvalidRequest {
		t.Fatalf("error code = %d, want %d", m.Error.Code, codeInvalidRequest)
	}
}

func TestNewSessionStartsThread(t *testing.T) {
	f := newFixture(t)
	sessionID := f.session()
	call := f.d.await("thread/start")
	if params := paramsOf[rpc.ThreadStartParams](t, call); params.Cwd != "/work" {
		t.Fatalf("thread cwd = %q, want /work", params.Cwd)
	}
	if sessionID != "t1" {
		t.Fatalf("sessionId = %q, want t1", sessionID)
	}
}

func TestNewSessionMCPServersNotice(t *testing.T) {
	f := newFixture(t)
	f.initialize()
	servers := []any{map[string]any{"type": "stdio", "name": "x", "command": "true", "args": []any{}, "env": []any{}}}
	id := f.ed.request("session/new", map[string]any{"cwd": "/work", "mcpServers": servers})
	var got sessionResult
	f.ed.result(id, &got)
	if got.SessionID == "" {
		t.Fatal("session/new returned no session id")
	}
	u := f.ed.update()
	if u.SessionID != got.SessionID || u.Update["sessionUpdate"] != kindAgentChunk {
		t.Fatalf("update = %+v", u)
	}
	content, _ := u.Update["content"].(map[string]any)
	if content["text"] != mcpNotice {
		t.Fatalf("notice text = %v, want %q", content["text"], mcpNotice)
	}
}

func TestPromptStreamsAndCompletes(t *testing.T) {
	f := newFixture(t)
	sessionID := f.session()
	id := f.prompt(sessionID, promptText("hello"))
	call := f.d.await("turn/start")
	params := paramsOf[rpc.TurnStartParams](t, call)
	if len(params.Input) != 1 || params.Input[0].Type != "text" || params.Input[0].Text != "hello" {
		t.Fatalf("turn input = %+v", params.Input)
	}
	f.d.notify("item/agentMessage/delta", rpc.AgentMessageDeltaNotification{ThreadID: "t1", TurnID: "u1", ItemID: "m1", Delta: "he"})
	u := f.ed.update()
	if u.SessionID != sessionID || u.Update["sessionUpdate"] != kindAgentChunk {
		t.Fatalf("delta update = %+v", u)
	}
	f.d.notify("item/completed", rpc.ItemCompletedNotification{ThreadID: "t1", TurnID: "u1", Item: rpc.Item{Type: itemAgent, ID: "m1", Text: "hello world", Status: "completed"}})
	f.d.notify("turn/completed", rpc.TurnCompletedNotification{ThreadID: "t1", Turn: rpc.Turn{ID: "u1", Status: "completed"}})
	var result promptResult
	f.ed.result(id, &result)
	if result.StopReason != stopEndTurn {
		t.Fatalf("stopReason = %q, want %q", result.StopReason, stopEndTurn)
	}
}

func TestPromptUnstreamedItemEmitted(t *testing.T) {
	f := newFixture(t)
	sessionID := f.session()
	id := f.prompt(sessionID, promptText("hi"))
	f.d.await("turn/start")
	f.d.notify("item/completed", rpc.ItemCompletedNotification{ThreadID: "t1", TurnID: "u1", Item: rpc.Item{Type: itemAgent, ID: "m1", Text: "whole reply", Status: "completed"}})
	u := f.ed.update()
	content, _ := u.Update["content"].(map[string]any)
	if content["text"] != "whole reply" {
		t.Fatalf("unstreamed item text = %v", content["text"])
	}
	f.d.notify("turn/completed", rpc.TurnCompletedNotification{ThreadID: "t1", Turn: rpc.Turn{ID: "u1", Status: "completed"}})
	var result promptResult
	f.ed.result(id, &result)
}

func TestPromptImageBlock(t *testing.T) {
	f := newFixture(t)
	sessionID := f.session()
	id := f.prompt(sessionID, map[string]any{"type": "image", "data": "QUJD", "mimeType": "image/png"})
	call := f.d.await("turn/start")
	params := paramsOf[rpc.TurnStartParams](t, call)
	if len(params.Input) != 1 || params.Input[0].Type != "image" || params.Input[0].URL != "data:image/png;base64,QUJD" {
		t.Fatalf("turn input = %+v", params.Input)
	}
	f.d.notify("turn/completed", rpc.TurnCompletedNotification{ThreadID: "t1", Turn: rpc.Turn{ID: "u1", Status: "completed"}})
	var result promptResult
	f.ed.result(id, &result)
	if result.StopReason != stopEndTurn {
		t.Fatalf("stopReason = %q", result.StopReason)
	}
}

func TestPromptUnknownSession(t *testing.T) {
	f := newFixture(t)
	f.session()
	id := f.prompt("nope", promptText("hi"))
	err := f.ed.failure(id)
	if err.Code != codeInvalidParams {
		t.Fatalf("code = %d, want %d", err.Code, codeInvalidParams)
	}
}

func TestPromptUnsupportedBlock(t *testing.T) {
	f := newFixture(t)
	sessionID := f.session()
	id := f.prompt(sessionID, map[string]any{"type": "audio", "data": "QUJD", "mimeType": "audio/wav"})
	err := f.ed.failure(id)
	if err.Code != codeInvalidParams {
		t.Fatalf("code = %d, want %d", err.Code, codeInvalidParams)
	}
	if f.d.count("turn/start") != 0 {
		t.Fatal("an unsupported block must not reach the daemon")
	}
}

func TestPromptFailedTurn(t *testing.T) {
	f := newFixture(t)
	sessionID := f.session()
	id := f.prompt(sessionID, promptText("boom"))
	f.d.await("turn/start")
	f.d.notify("turn/completed", rpc.TurnCompletedNotification{ThreadID: "t1", Turn: rpc.Turn{ID: "u1", Status: "failed", Error: &rpc.TurnError{Message: "model exploded"}}})
	err := f.ed.failure(id)
	if err.Code != codeInternal || err.Message != "model exploded" {
		t.Fatalf("error = %+v", err)
	}
}

func TestConcurrentPromptRejected(t *testing.T) {
	f := newFixture(t)
	sessionID := f.session()
	first := f.prompt(sessionID, promptText("one"))
	f.d.await("turn/start")
	second := f.prompt(sessionID, promptText("two"))
	err := f.ed.failure(second)
	if err.Code != codeInvalidRequest {
		t.Fatalf("code = %d, want %d", err.Code, codeInvalidRequest)
	}
	f.d.notify("turn/completed", rpc.TurnCompletedNotification{ThreadID: "t1", Turn: rpc.Turn{ID: "u1", Status: "completed"}})
	var result promptResult
	f.ed.result(first, &result)
}

func TestCancelInterruptsActiveTurn(t *testing.T) {
	f := newFixture(t)
	sessionID := f.session()
	promptID := f.prompt(sessionID, promptText("hi"))
	f.d.await("turn/start")
	f.ed.notify("session/cancel", map[string]any{"sessionId": sessionID})
	interrupt := f.d.await("turn/interrupt")
	params := paramsOf[rpc.TurnInterruptParams](t, interrupt)
	if params.ThreadID != "t1" || params.TurnID != "u1" {
		t.Fatalf("interrupt params = %+v", params)
	}
	f.d.notify("turn/completed", rpc.TurnCompletedNotification{ThreadID: "t1", Turn: rpc.Turn{ID: "u1", Status: "interrupted"}})
	var result promptResult
	f.ed.result(promptID, &result)
	if result.StopReason != stopCancelled {
		t.Fatalf("stopReason = %q, want %q", result.StopReason, stopCancelled)
	}
}

func TestCancelUnknownSessionIsNoop(t *testing.T) {
	f := newFixture(t)
	f.session()
	f.ed.notify("session/cancel", map[string]any{"sessionId": "ghost"})
	time.Sleep(100 * time.Millisecond)
	if n := f.d.count("turn/interrupt"); n != 0 {
		t.Fatalf("turn/interrupt calls = %d, want 0", n)
	}
}

func TestSetConfigOptionRefusals(t *testing.T) {
	f := newFixture(t)
	sessionID := f.session()
	id := f.ed.request("session/set_config_option", map[string]any{"sessionId": sessionID, "configId": "model", "type": "id", "value": "x"})
	err := f.ed.failure(id)
	if err.Code != codeMethodNotFound || err.Message != modelRefusal {
		t.Fatalf("error = %+v, want code %d message %q", err, codeMethodNotFound, modelRefusal)
	}
	id = f.ed.request("session/set_config_option", map[string]any{"sessionId": sessionID, "configId": "theme", "type": "id", "value": "x"})
	err = f.ed.failure(id)
	if err.Code != codeMethodNotFound {
		t.Fatalf("code = %d, want %d", err.Code, codeMethodNotFound)
	}
}

func TestSessionListFiltersByCwd(t *testing.T) {
	f := newFixture(t)
	f.session()
	f.d.script("thread/list", func(jsontext.Value) (any, error) {
		return rpc.ThreadListResponse{Data: []rpc.Thread{
			{ID: "a", Cwd: "/work", Name: "alpha", UpdatedAt: 1700000000},
			{ID: "b", Cwd: "/other", Preview: "beta"},
		}}, nil
	})
	id := f.ed.request("session/list", map[string]any{"cwd": "/work", "cursor": nil})
	var got listResult
	f.ed.result(id, &got)
	if len(got.Sessions) != 1 || got.Sessions[0].SessionID != "a" {
		t.Fatalf("sessions = %+v", got.Sessions)
	}
	info := got.Sessions[0]
	if info.Title != "alpha" || info.UpdatedAt != "2023-11-14T22:13:20Z" {
		t.Fatalf("session info = %+v", info)
	}
}

func TestSessionListEmptyCwdKeepsAll(t *testing.T) {
	f := newFixture(t)
	f.session()
	f.d.script("thread/list", func(jsontext.Value) (any, error) {
		return rpc.ThreadListResponse{Data: []rpc.Thread{
			{ID: "a", Cwd: "/work"},
			{ID: "b", Cwd: "/other"},
		}}, nil
	})
	id := f.ed.request("session/list", map[string]any{"cwd": "", "cursor": ""})
	var got listResult
	f.ed.result(id, &got)
	if len(got.Sessions) != 2 {
		t.Fatalf("sessions = %+v", got.Sessions)
	}
}

func TestLoadSessionResumesAndReplays(t *testing.T) {
	f := newFixture(t)
	f.session()
	f.d.script("thread/resume", func(jsontext.Value) (any, error) {
		return rpc.ThreadResumeResponse{Thread: rpc.Thread{ID: "t9"}}, nil
	})
	f.d.script("thread/turns/list", func(jsontext.Value) (any, error) {
		return rpc.ThreadTurnsListResponse{Data: []rpc.Turn{{
			ID: "u0",
			Items: []rpc.Item{
				{Type: itemUser, ID: "i1", Text: "earlier question"},
				{Type: itemAgent, ID: "i2", Text: "earlier answer"},
				{Type: itemCommand, ID: "i3", Text: "ignored tool"},
			},
		}}}, nil
	})
	id := f.ed.request("session/load", map[string]any{"sessionId": "t9", "cwd": "/work", "mcpServers": []any{}})
	resume := f.d.await("thread/resume")
	if params := paramsOf[rpc.ThreadResumeParams](t, resume); params.ThreadID != "t9" || !params.ExcludeTurns {
		t.Fatalf("resume params = %+v", params)
	}
	first := f.ed.update()
	if first.SessionID != "t9" || !reflect.DeepEqual(first.Update, map[string]any{
		"sessionUpdate": kindUserChunk,
		"content":       map[string]any{"type": "text", "text": "earlier question"},
	}) {
		t.Fatalf("first replay update = %+v", first)
	}
	second := f.ed.update()
	if second.SessionID != "t9" || !reflect.DeepEqual(second.Update, map[string]any{
		"sessionUpdate": kindAgentChunk,
		"content":       map[string]any{"type": "text", "text": "earlier answer"},
	}) {
		t.Fatalf("second replay update = %+v", second)
	}
	f.ed.result(id, &map[string]any{})
	if f.d.count("thread/turns/list") != 1 {
		t.Fatal("a single replay page must not loop")
	}
}

func TestToolCallNotifications(t *testing.T) {
	f := newFixture(t)
	sessionID := f.session()
	promptID := f.prompt(sessionID, promptText("run it"))
	f.d.await("turn/start")
	f.d.notify("item/started", rpc.ItemStartedNotification{ThreadID: "t1", TurnID: "u1", Item: rpc.Item{Type: itemCommand, ID: "c1", Command: "ls -la", Status: "inProgress"}})
	u := f.ed.update()
	if u.Update["sessionUpdate"] != "tool_call" {
		t.Fatalf("started update = %+v", u.Update)
	}
	if u.Update["kind"] != "execute" || u.Update["status"] != "in_progress" || u.Update["title"] != "ls -la" {
		t.Fatalf("tool call = %+v", u.Update)
	}
	f.d.notify("item/completed", rpc.ItemCompletedNotification{ThreadID: "t1", TurnID: "u1", Item: rpc.Item{Type: itemCommand, ID: "c1", Command: "ls -la", Status: "completed", Output: "done"}})
	u = f.ed.update()
	if u.Update["sessionUpdate"] != "tool_call_update" || u.Update["status"] != "completed" {
		t.Fatalf("completed update = %+v", u.Update)
	}
	f.d.notify("turn/completed", rpc.TurnCompletedNotification{ThreadID: "t1", Turn: rpc.Turn{ID: "u1", Status: "completed"}})
	var result promptResult
	f.ed.result(promptID, &result)
}

func TestReasoningDeltaStreamsThought(t *testing.T) {
	f := newFixture(t)
	sessionID := f.session()
	promptID := f.prompt(sessionID, promptText("think"))
	f.d.await("turn/start")
	f.d.notify("item/reasoning/textDelta", rpc.ReasoningTextDeltaNotification{ThreadID: "t1", TurnID: "u1", ItemID: "r1", Delta: "hmm"})
	u := f.ed.update()
	if u.Update["sessionUpdate"] != kindThoughtChunk {
		t.Fatalf("thought update = %+v", u.Update)
	}
	f.d.notify("turn/completed", rpc.TurnCompletedNotification{ThreadID: "t1", Turn: rpc.Turn{ID: "u1", Status: "completed"}})
	var result promptResult
	f.ed.result(promptID, &result)
}

func TestTurnPlanAndUsage(t *testing.T) {
	f := newFixture(t)
	sessionID := f.session()
	promptID := f.prompt(sessionID, promptText("plan"))
	f.d.await("turn/start")
	f.d.notify("turn/plan/updated", rpc.TurnPlanUpdatedNotification{ThreadID: "t1", TurnID: "u1", Plan: []rpc.TurnPlanStep{{Step: "first", Status: "inProgress"}, {Step: "second", Status: "pending"}}})
	assertPlanUpdate(t, f.ed.update())
	cost := int64(1500000)
	f.d.notify("thread/tokenUsage/updated", rpc.ThreadTokenUsageUpdatedNotification{ThreadID: "t1", TurnID: "u1", TokenUsage: rpc.ThreadTokenUsage{
		Last:               rpc.TokenUsageBreakdown{TotalTokens: 42},
		ModelContextWindow: 1000,
		CostUsdMicros:      &cost,
	}})
	assertUsageUpdate(t, f.ed.update())
	f.d.notify("turn/completed", rpc.TurnCompletedNotification{ThreadID: "t1", Turn: rpc.Turn{ID: "u1", Status: "completed"}})
	var result promptResult
	f.ed.result(promptID, &result)
}

func assertPlanUpdate(t *testing.T, u updateNote) {
	t.Helper()
	if u.Update["sessionUpdate"] != "plan" {
		t.Fatalf("plan update = %+v", u.Update)
	}
	entries, _ := u.Update["entries"].([]any)
	if len(entries) != 2 {
		t.Fatalf("plan entries = %+v", u.Update)
	}
	first, _ := entries[0].(map[string]any)
	if first["status"] != "in_progress" || first["priority"] != "medium" {
		t.Fatalf("plan entry = %+v", first)
	}
}

func assertUsageUpdate(t *testing.T, u updateNote) {
	t.Helper()
	if u.Update["sessionUpdate"] != "usage_update" || u.Update["used"] != float64(42) || u.Update["size"] != float64(1000) {
		t.Fatalf("usage update = %+v", u.Update)
	}
	c, _ := u.Update["cost"].(map[string]any)
	if c["amount"] != 1.5 || c["currency"] != "USD" {
		t.Fatalf("cost = %+v", c)
	}
}

func TestRequestPermissionRoundTrip(t *testing.T) {
	f := newFixture(t)
	sessionID := f.session()
	promptID := f.prompt(sessionID, promptText("approve me"))
	f.d.await("turn/start")
	var approval struct {
		Decision string `json:"decision"`
	}
	answered := make(chan error, 1)
	go func() {
		answered <- f.d.conn.Call(t.Context(), "item/commandExecution/requestApproval",
			jsontext.Value(`{"threadId":"t1","turnId":"u1","itemId":"c1","command":"rm -rf /","reason":"cleanup"}`), &approval)
	}()
	perm := f.ed.next()
	if perm.Method != "session/request_permission" || len(perm.ID) == 0 {
		t.Fatalf("expected a permission request, got %+v", perm)
	}
	assertPermissionRequest(t, perm.Params, sessionID)
	f.ed.respond(perm.ID, map[string]any{"outcome": map[string]any{"outcome": "selected", "optionId": "allow_once"}})
	if err := <-answered; err != nil {
		t.Fatalf("daemon request: %v", err)
	}
	if approval.Decision != "accept" {
		t.Fatalf("approval decision = %q, want accept", approval.Decision)
	}
	f.d.notify("turn/completed", rpc.TurnCompletedNotification{ThreadID: "t1", Turn: rpc.Turn{ID: "u1", Status: "completed"}})
	var result promptResult
	f.ed.result(promptID, &result)
}

func assertPermissionRequest(t *testing.T, params jsontext.Value, sessionID string) {
	t.Helper()
	var req struct {
		SessionID string       `json:"sessionId"`
		ToolCall  toolCallNote `json:"toolCall"`
		Options   []struct {
			OptionID string `json:"optionId"`
			Kind     string `json:"kind"`
		} `json:"options"`
	}
	if err := json.Unmarshal(params, &req); err != nil {
		t.Fatal(err)
	}
	if req.SessionID != sessionID || req.ToolCall.Kind != "execute" || req.ToolCall.Title != "rm -rf /" {
		t.Fatalf("permission request = %+v", req)
	}
	if len(req.Options) != 2 {
		t.Fatalf("options = %+v", req.Options)
	}
}

func TestServeNeedsDialer(t *testing.T) {
	err := Serve(t.Context(), nil, io.Discard, Options{})
	if err == nil {
		t.Fatal("Serve without a dialer must fail")
	}
}
