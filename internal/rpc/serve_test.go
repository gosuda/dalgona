package rpc

import (
	"context"
	json "encoding/json/v2"
	"errors"
	"io"
	"maps"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/auth/oauth"
	"github.com/gosuda/dalgona/internal/config"
)

type textInput struct {
	Type string `json:"type"`
	Text string `json:"text"`
}

func prompt(threadID, text string) map[string]any {
	return map[string]any{"threadId": threadID, "input": []textInput{{Type: "text", Text: text}}}
}

func startThread(t *testing.T, r *rig, c *wireClient, extra map[string]any) string {
	t.Helper()
	params := map[string]any{"cwd": r.cwd}
	maps.Copy(params, extra)
	var started ThreadStartResponse
	c.ok("thread/start", params, &started)
	note := c.await("thread/started", nil)
	if id := fieldOf(t, note.Params, "thread", "id"); id != started.Thread.ID || id == "" {
		t.Fatalf("thread/started id %q, response id %q", id, started.Thread.ID)
	}
	return started.Thread.ID
}

func startTurn(t *testing.T, c *wireClient, threadID, text string) string {
	t.Helper()
	var response TurnStartResponse
	c.ok("turn/start", prompt(threadID, text), &response)
	if response.Turn.ID == "" || response.Turn.Status != statusInProgress {
		t.Fatalf("turn/start returned %+v", response.Turn)
	}
	return response.Turn.ID
}

type turnLog struct {
	deltas    strings.Builder
	completed []Item
	status    string
	turnID    string
	usage     bool
}

func readTurn(t *testing.T, notes []message) turnLog {
	t.Helper()
	var log turnLog
	for _, m := range notes {
		switch m.Method {
		case "item/agentMessage/delta":
			log.deltas.WriteString(fieldOf(t, m.Params, "delta"))
		case "item/completed":
			log.completed = append(log.completed, itemOf(t, m))
		case "thread/tokenUsage/updated":
			log.usage = true
		case "turn/completed":
			log.status = fieldOf(t, m.Params, "turn", "status")
			log.turnID = fieldOf(t, m.Params, "turn", "id")
		}
	}
	return log
}

func expect(t *testing.T, ok bool, what string, got any) {
	t.Helper()
	if !ok {
		t.Fatalf("%s: %+v", what, got)
	}
}

func TestServeTurnEmitsSchemaValidItems(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	r.llm.push(llmReply{body: sseCall("call-1", "exec", `{"command":"echo hi"}`)}, llmReply{body: sseText("hel", "lo")})
	c := r.connect()
	var hello InitializeResult
	c.ok("initialize", map[string]any{"clientInfo": map[string]string{"name": "test", "version": "0"}}, &hello)
	expect(t, strings.HasPrefix(hello.UserAgent, "dal/") && hello.CodexHome == r.home, "initialize", hello)
	threadID := startThread(t, r, c, nil)
	turnID := startTurn(t, c, threadID, "run it")
	log := readTurn(t, c.collect("turn/completed"))
	expect(t, log.status == statusCompleted && log.turnID == turnID, "turn/completed", log)
	kinds := make([]string, 0, len(log.completed))
	for _, it := range log.completed {
		kinds = append(kinds, it.Type)
	}
	expect(t, strings.Join(kinds, ",") == "userMessage,commandExecution,agentMessage", "completed items", kinds)
	checkExecItems(t, log)
	var read ThreadReadResponse
	c.ok("thread/read", map[string]any{"threadId": threadID, "includeTurns": true}, &read)
	expect(t, len(read.Thread.Turns) == 1 && read.Thread.Turns[0].ID == turnID && read.Thread.Turns[0].Status == statusCompleted, "thread/read turns", read.Thread.Turns)
	expect(t, reflect.DeepEqual(read.Thread.Turns[0].Items, log.completed), "thread/read items equal live items", read.Thread.Turns[0].Items)
	expect(t, read.Thread.Preview == "run it" && read.Thread.Status.Type == "idle", "thread/read thread", read.Thread)
}

func TestServeThreadMethods(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	turnID := startTurn(t, c, threadID, "first")
	c.await("turn/completed", nil)

	var list ThreadListResponse
	c.ok("thread/list", map[string]any{}, &list)
	expect(t, len(list.Data) == 1 && list.Data[0].ID == threadID && list.Data[0].Preview == "first", "thread/list", list.Data)
	c.ok("thread/name/set", map[string]any{"threadId": threadID, "name": "Demo"}, nil)
	name := fieldOf(t, c.await("thread/name/updated", nil).Params, "threadName")
	expect(t, name == "Demo", "thread/name/updated", name)
	var tree ThreadTreeResponse
	c.ok("dalgona/thread/tree", map[string]any{"threadId": threadID}, &tree)
	expect(t, len(tree.Turns) == 1 && tree.Turns[0].TurnID == turnID && tree.Turns[0].Active && tree.Turns[0].Preview == "first", "tree", tree.Turns)
	c.fails("thread/revert", map[string]any{"threadId": threadID, "beforeTurnId": "nope"}, codeInvalidParams, "turn nope is not on the active path")
	c.fails("dalgona/thread/leaf/set", map[string]any{"threadId": threadID, "turnId": "nope"}, codeInvalidParams, "unknown turn nope")
	c.ok("thread/revert", map[string]any{"threadId": threadID, "beforeTurnId": turnID}, nil)
	c.await("thread/reverted", nil)
	var read ThreadReadResponse
	c.ok("thread/read", map[string]any{"threadId": threadID, "includeTurns": true}, &read)
	expect(t, len(read.Thread.Turns) == 0, "turns after revert", read.Thread.Turns)
	c.ok("dalgona/thread/leaf/set", map[string]any{"threadId": threadID, "turnId": turnID}, nil)
	var fork ThreadStartResponse
	c.ok("thread/fork", map[string]any{"threadId": threadID}, &fork)
	expect(t, fork.Thread.ID != threadID && fork.Thread.ForkedFromID == threadID && len(fork.Thread.Turns) == 1, "fork", fork.Thread)
	c.ok("thread/archive", map[string]any{"threadId": fork.Thread.ID}, nil)
	c.await("thread/archived", nil)
	c.ok("thread/list", map[string]any{"archived": true}, &list)
	expect(t, len(list.Data) == 1 && list.Data[0].ID == fork.Thread.ID, "archived list", list.Data)
	c.ok("thread/unarchive", map[string]any{"threadId": fork.Thread.ID}, nil)
	c.fails("thread/resume", map[string]any{"threadId": "missing"}, codeInvalidParams, "unknown thread missing")
	c.fails("no/such/method", map[string]any{}, codeMethodNotFound, "Method not found")
}

func TestServeConfigAccountAndLists(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	var models ModelListResponse
	c.ok("model/list", map[string]any{}, &models)
	expect(t, len(models.Data) == 1 && models.Data[0].ID == "openai/gpt-test" && models.Data[0].IsDefault, "model/list", models.Data)
	started := checkConfigWrites(t, r, c)
	checkAccount(t, c)
	var skills SkillsListResponse
	c.ok("skills/list", map[string]any{}, &skills)
	expect(t, len(skills.Data) == 0, "dal skills", skills.Data)
	var keywords []Keyword
	c.ok("dalgona/keywords/list", map[string]any{}, &keywords)
	expect(t, len(keywords) > 0 && keywords[0].Word != "", "keywords", keywords)
	c.fails("dalgona/advisor/status", map[string]any{"threadId": started.Thread.ID}, codeNotAvailable, "advisor runtime is not available")
	c.fails("thread/start", map[string]any{"approvalPolicy": "sometimes"}, codeInvalidParams, "unsupported approvalPolicy sometimes")
}

func checkConfigWrites(t *testing.T, r *rig, c *wireClient) ThreadStartResponse {
	t.Helper()
	var cfg ConfigReadResponse
	c.ok("config/read", map[string]any{}, &cfg)
	expect(t, cfg.Config.Model == "openai/gpt-test" && cfg.Config.ApprovalPolicy == "on-request" && cfg.Config.SandboxMode == "danger-full-access", "config/read before writes", cfg.Config)
	c.ok("config/value/write", map[string]any{"keyPath": "model", "value": "openai/gpt-other", "mergeStrategy": "replace"}, nil)
	c.ok("config/value/write", map[string]any{"keyPath": "sandbox_mode", "value": "workspace-write", "mergeStrategy": "replace"}, nil)
	c.ok("config/value/write", map[string]any{"keyPath": "approval_policy", "value": "untrusted", "mergeStrategy": "replace"}, nil)
	c.fails("config/value/write", map[string]any{"keyPath": "approval_policy", "value": "sometimes", "mergeStrategy": "replace"}, codeInvalidParams, "unsupported approvalPolicy sometimes")
	c.ok("config/read", map[string]any{}, &cfg)
	expect(t, cfg.Config.Model == "openai/gpt-other" && cfg.Config.ApprovalPolicy == "untrusted" && cfg.Config.SandboxMode == "workspace-write", "config/read after writes", cfg.Config)
	var started ThreadStartResponse
	c.ok("thread/start", map[string]any{"cwd": r.cwd}, &started)
	expect(t, started.Model == "openai/gpt-other" && started.ApprovalPolicy == "untrusted" && started.Sandbox.Type == effectiveWorkspaceWriteType(), "thread/start after config write", started)
	return started
}

func TestServeRunningThreadErrors(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	gate := make(chan struct{})
	r.llm.push(llmReply{body: sseText("slow"), gate: gate})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	turnID := startTurn(t, c, threadID, "wait")
	c.ok("initialize", map[string]any{"clientInfo": map[string]string{"name": "test", "version": "0"}}, nil)
	running := "thread " + threadID + " is running"
	c.fails("thread/revert", map[string]any{"threadId": threadID, "beforeTurnId": turnID}, codeInvalidRequest, running)
	c.fails("thread/fork", map[string]any{"threadId": threadID}, codeInvalidRequest, running)
	c.fails("turn/start", prompt(threadID, "again"), codeInvalidRequest, running)
	c.fails("turn/steer", map[string]any{"threadId": threadID, "expectedTurnId": "other", "input": []textInput{{Type: "text", Text: "x"}}}, codeInvalidRequest, "turn other is not running")
	var steer TurnSteerResponse
	c.ok("turn/steer", map[string]any{"threadId": threadID, "expectedTurnId": turnID, "input": []textInput{{Type: "text", Text: "also"}}}, &steer)
	expect(t, steer.TurnID == turnID, "turn/steer turn", steer.TurnID)
	close(gate)
	log := readTurn(t, c.collect("turn/completed"))
	expect(t, log.status == statusCompleted, "turn status", log.status)
}

func TestServeAskModeApprovalRoundTrip(t *testing.T) {
	for _, tc := range []struct {
		decision string
		status   string
	}{{"accept", statusCompleted}, {"decline", statusDeclined}} {
		t.Run(tc.decision, func(t *testing.T) { approvalRoundTrip(t, tc.decision, tc.status) })
	}
}

func approvalRoundTrip(t *testing.T, decision, status string) {
	r := newRig(t, config.ApprovalAuto)
	r.llm.push(llmReply{body: sseCall("call-a", "exec", `{"command":"echo approved"}`)}, llmReply{body: sseText("after")})
	c := r.connect()
	threadID := startThread(t, r, c, map[string]any{"approvalPolicy": "untrusted"})
	startTurn(t, c, threadID, "go")
	var request message
	select {
	case request = <-c.requests:
	case <-time.After(waitLimit):
		t.Fatal("no approval request")
	}
	expect(t, request.Method == "item/commandExecution/requestApproval" && fieldOf(t, request.Params, "itemId") == "call-a", "approval request", string(request.Params))
	c.respond(request.ID, `{"decision":"`+decision+`"}`)
	resolved := c.await("serverRequest/resolved", nil)
	var id string
	err := json.Unmarshal(request.ID, &id)
	expect(t, err == nil && fieldOf(t, resolved.Params, "requestId") == id, "serverRequest/resolved", string(resolved.Params))
	command := findItem(readTurn(t, c.collect("turn/completed")).completed, "call-a")
	expect(t, command.Status == status, "command item status", command)
}

func findItem(items []Item, id string) Item {
	for _, it := range items {
		if it.ID == id {
			return it
		}
	}
	return Item{}
}

func checkExecItems(t *testing.T, log turnLog) {
	t.Helper()
	user, command, agentText := log.completed[0], log.completed[1], log.completed[2]
	expect(t, user.Text == "run it", "user item", user)
	expect(t, command.ID == "call-1" && command.Command == "echo hi" && command.Status == statusCompleted, "command item", command)
	expect(t, strings.Contains(command.Output, "hi"), "command output", command.Output)
	expect(t, command.ExitCode != nil && *command.ExitCode == 0, "command exit code", command.ExitCode)
	expect(t, log.deltas.String() == "hello" && agentText.Text == "hello", "streamed text", log.deltas.String()+" / "+agentText.Text)
	expect(t, log.usage, "thread/tokenUsage/updated", log.usage)
}

func checkAccount(t *testing.T, c *wireClient) {
	t.Helper()
	var account AccountReadResponse
	c.ok("account/read", map[string]any{}, &account)
	expect(t, account.Account == nil, "account before login", account.Account)
	c.ok("account/login/start", map[string]any{"type": "apiKey", "apiKey": "sk-test"}, nil)
	c.ok("account/read", map[string]any{}, &account)
	expect(t, account.Account != nil && account.Account.Type == "apiKey", "account after login", account.Account)
	c.ok("account/logout", map[string]any{}, nil)
	c.ok("account/login/cancel", map[string]any{"loginId": "none"}, nil)
}

func TestServeArchiveHookRunsOnlyForIdleThreads(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	gate := make(chan struct{})
	r.llm.push(llmReply{body: sseText("slow"), gate: gate})
	var hooked []string
	var mu sync.Mutex
	c := r.connectWith(ServeOptions{BeforeArchive: func(_ context.Context, id string) error {
		mu.Lock()
		defer mu.Unlock()
		hooked = append(hooked, id)
		return nil
	}})
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "wait")
	c.fails("thread/archive", map[string]any{"threadId": threadID}, codeInvalidRequest, "thread "+threadID+" is running")
	close(gate)
	c.await("turn/completed", nil)
	c.ok("thread/archive", map[string]any{"threadId": threadID}, nil)
	mu.Lock()
	defer mu.Unlock()
	expect(t, len(hooked) == 1 && hooked[0] == threadID, "archive hook calls", hooked)
}

func TestServeDeviceLoginReportsCodeAndURL(t *testing.T) {
	device := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, `{"device_auth_id":"dev-1","user_code":"ABCD-1234","verification_uri":"https://auth.example/verify","interval":60}`)
	}))
	t.Cleanup(device.Close)
	original := chatGPTProvider
	t.Cleanup(func() { chatGPTProvider = original })
	chatGPTProvider = func() (oauth.Provider, bool) {
		provider, ok := original()
		provider.DeviceURL = device.URL
		return provider, ok
	}
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	var login LoginAccountResponse
	c.ok("account/login/start", map[string]any{"type": "chatgptDeviceCode"}, &login)
	expect(t, login.Type == "chatgptDeviceCode" && login.UserCode == "ABCD-1234" && login.VerificationURL == "https://auth.example/verify", "device login", login)
	var cancelled CancelLoginAccountResponse
	c.ok("account/login/cancel", map[string]any{"loginId": login.LoginID}, &cancelled)
	expect(t, cancelled.Status == "canceled", "login cancel", cancelled)
	completed := c.await("account/login/completed", nil)
	expect(t, fieldOf(t, completed.Params, "success") == "false" && fieldOf(t, completed.Params, "loginId") == login.LoginID, "login completed", string(completed.Params))
}

func TestServeQualifiesBareCodexModel(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	var started ThreadStartResponse
	c.ok("thread/start", map[string]any{"cwd": r.cwd, "model": "gpt-test", "approvalsReviewer": "auto_review", "config": map[string]any{"model_reasoning_effort": "high"}}, &started)
	expect(t, started.Model == "openai/gpt-test" && started.ModelProvider == "openai", "bare model start", started)
	c.await("thread/started", nil)
	startTurn(t, c, started.Thread.ID, "hi")
	log := readTurn(t, c.collect("turn/completed"))
	expect(t, log.status == statusCompleted, "turn with bare model", log.status)
	c.ok("thread/start", map[string]any{"cwd": r.cwd, "model": "gpt-x", "modelProvider": "anthropic"}, &started)
	expect(t, started.Model == "anthropic/gpt-x", "explicit modelProvider", started.Model)
}

func TestServeArchiveHookBlocksTurnsAndRollsBack(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	var submitErr error
	c := r.connectWith(ServeOptions{BeforeArchive: func(ctx context.Context, id string) error {
		thread, err := r.host.Resume(ctx, id)
		if err != nil {
			return err
		}
		submitErr = thread.Submit(ctx, agent.Input{Kind: agent.InputPrompt, Text: "sneak in"})
		return errors.New("box teardown failed")
	}})
	threadID := startThread(t, r, c, nil)
	c.fails("thread/archive", map[string]any{"threadId": threadID}, codeInternal, "archive hook: box teardown failed")
	expect(t, errors.Is(submitErr, agent.ErrArchived), "turn start during the archive hook", submitErr)
	var list ThreadListResponse
	c.ok("thread/list", map[string]any{"archived": true}, &list)
	expect(t, len(list.Data) == 0, "archived threads after a failed hook", list.Data)
	startTurn(t, c, threadID, "after rollback")
	log := readTurn(t, c.collect("turn/completed"))
	expect(t, log.status == statusCompleted, "turn after rollback", log.status)
}

func TestServeAnswersRequestsReceivedBeforeClientEOF(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	client, served := serveUnix(t, r)
	request := `{"id":1,"method":"initialize","params":{"clientInfo":{"name":"x","version":"1"}}}` + "\n" + `{"id":2,"method":"thread/list","params":{}}` + "\n"
	if _, err := io.WriteString(client, request); err != nil {
		t.Fatal(err)
	}
	if err := client.CloseWrite(); err != nil {
		t.Fatal(err)
	}
	replies, err := io.ReadAll(client)
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range []string{`"id":1,`, `"id":2,`} {
		expect(t, strings.Contains(string(replies), id), "reply after client EOF", string(replies))
	}
	select {
	case serveErr := <-served:
		expect(t, serveErr == nil, "Serve result", serveErr)
	case <-time.After(waitLimit):
		t.Fatal("Serve did not return after draining")
	}
}

func serveUnix(t *testing.T, r *rig) (*net.UnixConn, <-chan error) {
	t.Helper()
	dir, err := os.MkdirTemp("", "rpc")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	listener, err := net.Listen("unix", filepath.Join(dir, "s"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	served := make(chan error, 1)
	go func() {
		conn, acceptErr := listener.Accept()
		if acceptErr != nil {
			served <- acceptErr
			return
		}
		served <- Serve(t.Context(), conn, r.host)
	}()
	client, err := net.DialUnix("unix", nil, &net.UnixAddr{Name: filepath.Join(dir, "s"), Net: "unix"})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = client.Close() })
	return client, served
}

func TestServeTurnApprovalNeverLowersThread(t *testing.T) {
	for _, tc := range []struct {
		name   string
		thread map[string]any
		turn   string
	}{
		{"ask thread, on-request turn", map[string]any{"approvalPolicy": "untrusted"}, "on-request"},
		{"ask thread, never turn", map[string]any{"approvalPolicy": "untrusted"}, "never"},
		{"auto thread, untrusted turn", nil, "untrusted"},
	} {
		t.Run(tc.name, func(t *testing.T) { turnApprovalReachesClient(t, tc.thread, tc.turn) })
	}
}

func turnApprovalReachesClient(t *testing.T, threadParams map[string]any, turnPolicy string) {
	r := newRig(t, config.ApprovalAuto)
	r.llm.push(llmReply{body: sseCall("call-t", "exec", `{"command":"echo turn"}`)}, llmReply{body: sseText("after")})
	c := r.connect()
	threadID := startThread(t, r, c, threadParams)
	params := prompt(threadID, "go")
	params["approvalPolicy"] = turnPolicy
	params["sandboxPolicy"] = map[string]any{"type": "readOnly", "networkAccess": false}
	var response TurnStartResponse
	c.ok("turn/start", params, &response)
	var request message
	select {
	case request = <-c.requests:
	case <-time.After(waitLimit):
		t.Fatal("no approval request reached the client")
	}
	expect(t, request.Method == "item/commandExecution/requestApproval" && fieldOf(t, request.Params, "itemId") == "call-t", "approval request", string(request.Params))
	c.respond(request.ID, `{"decision":"accept"}`)
	command := findItem(readTurn(t, c.collect("turn/completed")).completed, "call-t")
	expect(t, command.Status == statusCompleted, "command after approval", command)
}

func TestSettingKeyRejectsBadEffort(t *testing.T) {
	if _, _, err := settingKey("model_reasoning_effort", "loud"); err == nil {
		t.Fatal("loud effort accepted")
	}
	if _, _, err := settingKey("model_reasoning_effort", 3); err == nil {
		t.Fatal("numeric effort accepted")
	}
}

func TestSettingKeyRejectsNonStringPolicy(t *testing.T) {
	if _, _, err := settingKey("approval_policy", true); err == nil {
		t.Fatal("bool approval accepted")
	}
	if _, _, err := settingKey("sandbox_mode", 7); err == nil {
		t.Fatal("numeric sandbox accepted")
	}
}

func TestSettingKeyRejectsUnmappedAndNumeric(t *testing.T) {
	if _, _, err := settingKey("compaction_threshold", 0.5); err == nil {
		t.Fatal("unmapped numeric key accepted")
	}
	if _, _, err := settingKey("wat", "x"); err == nil {
		t.Fatal("unknown key accepted")
	}
	if _, _, err := settingKey("model", 3); err == nil {
		t.Fatal("numeric model accepted")
	}
	if _, _, err := settingKey("", "x"); err == nil {
		t.Fatal("empty keyPath accepted")
	}
}

func checkDirectSettingKey(t *testing.T, keyPath, input, wantKey, wantValue string) {
	t.Helper()
	key, value, err := settingKey(keyPath, input)
	if err != nil || key != wantKey || value != wantValue {
		t.Fatalf("direct %s rejected: %q %v %v", keyPath, key, value, err)
	}
}

func TestSettingKeyAcceptsDirectKeys(t *testing.T) {
	checkDirectSettingKey(t, "model", "openai/gpt-x", "model", "openai/gpt-x")
	checkDirectSettingKey(t, "approval", "yolo", "approval", "yolo")
	checkDirectSettingKey(t, "sandbox", "off", "sandbox", string(config.SandboxOff))
	checkDirectSettingKey(t, "reasoning", "low", "reasoning", "low")
}

func TestConfigWriteRejectsUnsupportedKeepsFile(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	c.fails("config/value/write", map[string]any{"keyPath": "compaction_threshold", "value": 0.5}, codeInvalidParams, "unsupported keyPath compaction_threshold")
	c.fails("config/value/write", map[string]any{"keyPath": "model", "value": 3}, codeInvalidParams, "model must be a string")
	var cfg ConfigReadResponse
	c.ok("config/read", map[string]any{}, &cfg)
	if cfg.Config.Model != "openai/gpt-test" {
		t.Fatalf("rejected write changed memory: %+v", cfg.Config)
	}
}

func TestPlatformOSTypeClampsToSpec(t *testing.T) {
	for _, goos := range []string{"freebsd", "openbsd", "netbsd", "solaris"} {
		if got := platformOSType(goos); got != "linux" {
			t.Fatalf("platformOSType(%q)=%q, want linux", goos, got)
		}
	}
	if got := platformOSType("darwin"); got != "macos" {
		t.Fatalf("darwin=%q, want macos", got)
	}
	if got := platformOSType("windows"); got != "windows" {
		t.Fatalf("windows=%q, want windows", got)
	}
	if got := platformOSType("linux"); got != "linux" {
		t.Fatalf("linux=%q, want linux", got)
	}
}
