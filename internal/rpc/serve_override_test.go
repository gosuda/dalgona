package rpc

import (
	"bytes"
	"context"
	json "encoding/json/v2"
	"io"
	"net/http"
	"net/http/httptest"
	"slices"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/exec"
	"github.com/gosuda/dalgona/internal/tool/patch"
	"github.com/gosuda/dalgona/internal/tool/read"
)

type modelRequest struct {
	model  string
	tool   string
	effort string
}

type requestLog struct {
	mu       sync.Mutex
	requests []modelRequest
}

func (l *requestLog) seen() []modelRequest {
	l.mu.Lock()
	defer l.mu.Unlock()
	return slices.Clone(l.requests)
}

func (l *requestLog) add(body []byte) error {
	var request struct {
		Model     string `json:"model"`
		Reasoning struct {
			Effort string `json:"effort"`
		} `json:"reasoning"`
		Tools []struct {
			Name string `json:"name"`
		} `json:"tools"`
	}
	if err := json.Unmarshal(body, &request); err != nil {
		return err
	}
	seen := modelRequest{model: request.Model, effort: request.Reasoning.Effort}
	for _, item := range request.Tools {
		if item.Name == "patch" || item.Name == "apply_patch" {
			seen.tool = item.Name
		}
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	l.requests = append(l.requests, seen)
	return nil
}

func recordRequests(t *testing.T, r *rig) *requestLog {
	t.Helper()
	log := &requestLog{}
	f := &fakeLLM{}
	mux := http.NewServeMux()
	mux.HandleFunc("POST /responses", func(w http.ResponseWriter, req *http.Request) {
		body, err := io.ReadAll(req.Body)
		if err == nil {
			err = log.add(body)
		}
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		req.Body = io.NopCloser(bytes.NewReader(body))
		f.respond(w, req)
	})
	mux.HandleFunc("GET /models", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, `{"data":[{"id":"gpt-x"},{"id":"gpt-y"}]}`)
	})
	f.server = httptest.NewServer(mux)
	t.Cleanup(f.server.Close)
	r.llm = f
	return log
}

func editStyleTools(t *testing.T, r *rig) {
	t.Helper()
	styles := config.EditStyleSpec{Rows: []config.EditStyleRow{{Glob: "*gpt-y*", Style: "apply_patch"}}, Default: "replace"}
	r.cfg.Tools = func(_ context.Context, modelID, _ string) ([]tool.Tool, error) {
		return []tool.Tool{read.New(), exec.New(), patch.New(styles.EditStyle(modelID))}, nil
	}
	reopen(t, r)
}

func reopen(t *testing.T, r *rig) {
	t.Helper()
	if err := r.host.Close(); err != nil {
		t.Fatal(err)
	}
	r.openHost()
}

func schemaParams(t *testing.T, schema string, params any) any {
	t.Helper()
	raw, err := json.Marshal(params)
	if err != nil {
		t.Fatal(err)
	}
	validateLine(t, schema, raw)
	return params
}

func overrideTurn(t *testing.T, c *wireClient, params any) string {
	t.Helper()
	var response TurnStartResponse
	c.ok("turn/start", schemaParams(t, "v2/TurnStartParams.json", params), &response)
	expect(t, response.Turn.ID != "" && response.Turn.Status == statusInProgress, "turn/start", response.Turn)
	log := readTurn(t, c.collect("turn/completed"))
	expect(t, log.status == statusCompleted, "turn status", log.status)
	return response.Turn.ID
}

func expectSession(t *testing.T, c *wireClient, threadID string, want ThreadStartResponse) {
	t.Helper()
	var resumed ThreadStartResponse
	c.ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	got := [...]string{resumed.Model, resumed.ApprovalPolicy, resumed.Sandbox.Type}
	expect(t, got == [...]string{want.Model, want.ApprovalPolicy, want.Sandbox.Type}, "thread/resume settings", got)
	var read ThreadReadResponse
	c.ok("thread/read", map[string]any{"threadId": threadID}, &read)
	expect(t, read.Thread.Model == want.Model, "thread/read model", read.Thread.Model)
}

func TestTurnStartOverridesFollowActivePath(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	editStyleTools(t, r)
	requests := recordRequests(t, r)
	c := r.connect()
	threadID := startThread(t, r, c, map[string]any{"model": "openai/gpt-x"})
	plain := TurnStartParams{ThreadID: threadID, Input: []InputItem{{Type: "text", Text: "hi"}}}
	overrideTurn(t, c, plain)
	overrideID := overrideTurn(t, c, map[string]any{
		"threadId": threadID, "input": plain.Input, "model": "gpt-y",
		"approvalPolicy": map[string]any{"granular": map[string]any{"mcp_elicitations": true, "rules": true, "sandbox_approval": true}},
		"sandboxPolicy":  map[string]any{"type": "workspaceWrite", "networkAccess": false},
	})
	overrideTurn(t, c, plain)
	overridden := ThreadStartResponse{Model: "openai/gpt-y", ApprovalPolicy: "untrusted", Sandbox: SandboxPolicy{Type: effectiveWorkspaceWriteType()}}
	expectSession(t, c, threadID, overridden)
	var fork ThreadStartResponse
	c.ok("thread/fork", map[string]any{"threadId": threadID}, &fork)
	overrideTurn(t, c, TurnStartParams{ThreadID: fork.Thread.ID, Input: plain.Input})

	reopen(t, r)
	recovered := r.connect()
	overrideTurn(t, recovered, plain)
	expectSession(t, recovered, threadID, overridden)
	recovered.ok("thread/revert", map[string]any{"threadId": threadID, "beforeTurnId": overrideID}, nil)
	overrideTurn(t, recovered, plain)
	expectSession(t, recovered, threadID, ThreadStartResponse{Model: "openai/gpt-x", ApprovalPolicy: "never", Sandbox: SandboxPolicy{Type: "dangerFullAccess"}})

	want := []modelRequest{
		{model: "gpt-x", tool: "patch"},
		{model: "gpt-y", tool: "apply_patch"},
		{model: "gpt-y", tool: "apply_patch"},
		{model: "gpt-y", tool: "apply_patch"},
		{model: "gpt-y", tool: "apply_patch"},
		{model: "gpt-x", tool: "patch"},
	}
	expect(t, slices.Equal(requests.seen(), want), "model requests", requests.seen())
}

func TestThreadStartGranularApprovalAsks(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	r.llm.push(llmReply{body: sseCall("call-g", "exec", `{"command":"echo granular"}`)}, llmReply{body: sseText("after")})
	c := r.connect()
	params := schemaParams(t, "v2/ThreadStartParams.json", map[string]any{
		"cwd":            r.cwd,
		"approvalPolicy": map[string]any{"granular": map[string]any{"mcp_elicitations": false, "rules": true, "sandbox_approval": true}},
	})
	var started ThreadStartResponse
	c.ok("thread/start", params, &started)
	expect(t, started.ApprovalPolicy == "untrusted", "granular thread/start policy", started.ApprovalPolicy)
	startTurn(t, c, started.Thread.ID, "go")
	var request message
	select {
	case request = <-c.requests:
	case <-time.After(waitLimit):
		t.Fatal("no approval request under a granular approval policy")
	}
	expect(t, request.Method == "item/commandExecution/requestApproval", "granular approval request", request.Method)
	c.respond(request.ID, `{"decision":"decline"}`)
	command := findItem(readTurn(t, c.collect("turn/completed")).completed, "call-g")
	expect(t, command.Status == statusDeclined, "declined command", command)
}

func TestTurnStartRejectsUnknownOverrides(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	input := []InputItem{{Type: "text", Text: "x"}}
	c.fails("turn/start", TurnStartParams{ThreadID: threadID, Input: input, ApprovalPolicy: "sometimes"},
		codeInvalidParams, "unsupported approvalPolicy sometimes")
	c.fails("turn/start", TurnStartParams{ThreadID: threadID, Input: input, SandboxPolicy: &SandboxPolicy{Type: "externalSandbox"}},
		codeInvalidParams, "unsupported sandboxPolicy externalSandbox")
	c.fails("turn/start", map[string]any{"threadId": threadID, "input": input, "approvalPolicy": map[string]any{"rules": true}},
		codeInvalidParams, "approvalPolicy: an approval policy object must be granular")
}
