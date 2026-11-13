package llmstub

import (
	"bufio"
	"bytes"
	"context"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/ratchet"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

const (
	testKey    = "dummy"
	modelsJSON = `"models":[{"id":"qwen-test","context_length":32000}]`
	userHello  = `{"messages":[{"role":"user","content":"hello"}]}`
	chatRoute  = "POST /v1/chat/completions"
	modelRoute = "GET /v1/models"
)

func expectParseError(t *testing.T, src, want string) {
	t.Helper()
	_, err := Parse("test", []byte(src))
	if err == nil {
		t.Fatalf("Parse accepted %s", src)
	}
	if !strings.Contains(err.Error(), want) {
		t.Fatalf("error = %q, want it to contain %q", err, want)
	}
}

func mustParse(t *testing.T, src string) Script {
	t.Helper()
	script, err := Parse("test", []byte(src))
	if err != nil {
		t.Fatal(err)
	}
	return script
}

func serve(t *testing.T, h http.Handler, route, auth, body string) *httptest.ResponseRecorder {
	t.Helper()
	method, path, _ := strings.Cut(route, " ")
	req := httptest.NewRequestWithContext(t.Context(), method, path, strings.NewReader(body))
	if auth != "" {
		req.Header.Set("Authorization", auth)
	}
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	return rec
}

func TestParseRejectsStepWithTwoKinds(t *testing.T) {
	t.Parallel()
	expectParseError(t, `{`+modelsJSON+`,"turns":[{"id":"t1","when":{"user":"hi"},"steps":[{"text":"a","reasoning":"b"},{"finish":"stop"}]}]}`, "turn 1: step 1: needs exactly one of")
}

func TestParseRejectsStatusTurnWithSteps(t *testing.T) {
	t.Parallel()
	expectParseError(t, `{`+modelsJSON+`,"turns":[{"id":"t1","when":{"user":"hi"},"status":429,"body":{"error":"x"},"steps":[{"text":"a"}]}]}`, "a status turn takes no steps")
}

func TestParseRejectsRetryAfterWithoutRetryableStatus(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		turn string
	}{
		{name: "status 500", turn: `"status":500,"body":{"error":"x"}`},
		{name: "text turn", turn: `"steps":[{"text":"a"},{"finish":"stop"}]`},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			expectParseError(t, `{`+modelsJSON+`,"turns":[{"id":"t1","when":{"user":"hi"},"retry_after":"2",`+c.turn+`}]}`, "retry_after needs status 429 or 503")
		})
	}
}

func TestParseRejectsTurnWithoutTerminalStep(t *testing.T) {
	t.Parallel()
	expectParseError(t, `{`+modelsJSON+`,"turns":[{"id":"t1","when":{"user":"hi"},"steps":[{"text":"a"}]}]}`, "the last step MUST be finish, error_frame or drop")
}

func TestUnmatchedRequestIsTerminal400(t *testing.T) {
	t.Parallel()
	script := mustParse(t, `{`+modelsJSON+`,"turns":[{"id":"t1","when":{"user":"hello"},"steps":[{"finish":"stop"}]}]}`)
	h := NewHandler(script, Options{Key: testKey})
	rec := serve(t, h, chatRoute, "Bearer "+testKey, `{"messages":[{"role":"user","content":"other text"}]}`)
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("status = %d, want 400", rec.Code)
	}
	body := rec.Body.String()
	if !strings.Contains(body, `"code":"llmstub_unmatched"`) || !strings.Contains(body, "other text") {
		t.Fatalf("body = %s", body)
	}
}

func TestWrongBearerIs401(t *testing.T) {
	t.Parallel()
	script := mustParse(t, `{`+modelsJSON+`,"turns":[{"id":"t1","when":{"user":"hello"},"steps":[{"finish":"stop"}]}]}`)
	h := NewHandler(script, Options{Key: testKey})
	rec := serve(t, h, chatRoute, "Bearer wrong", userHello)
	if rec.Code != http.StatusUnauthorized || !strings.Contains(rec.Body.String(), `"code":"llmstub_auth"`) {
		t.Fatalf("status = %d, body = %s", rec.Code, rec.Body.String())
	}
}

func TestOnceTurnIsSpent(t *testing.T) {
	t.Parallel()
	script := mustParse(t, `{`+modelsJSON+`,"turns":[`+
		`{"id":"limited","when":{"user":"hello"},"once":true,"status":429,"retry_after":"2","body":{"error":{"code":"rate_limit","message":"slow down"}}},`+
		`{"id":"ok","when":{"user":"hello"},"steps":[{"text":"hi"},{"finish":"stop"}]}]}`)
	h := NewHandler(script, Options{Key: testKey})
	first := serve(t, h, chatRoute, "Bearer "+testKey, userHello)
	second := serve(t, h, chatRoute, "Bearer "+testKey, userHello)
	if first.Code != http.StatusTooManyRequests || first.Header().Get("Retry-After") != "2" {
		t.Fatalf("first status = %d, Retry-After = %q", first.Code, first.Header().Get("Retry-After"))
	}
	if second.Code != http.StatusOK || !strings.Contains(second.Body.String(), `"content":"hi"`) {
		t.Fatalf("second status = %d, body = %s", second.Code, second.Body.String())
	}
}

func TestDelayEndsOnClientCancel(t *testing.T) {
	t.Parallel()
	script := mustParse(t, `{`+modelsJSON+`,"turns":[{"id":"slow","when":{"user":"hello"},"steps":[{"text":"start"},{"delay_ms":60000},{"finish":"stop"}]}]}`)
	inner := NewHandler(script, Options{Key: testKey})
	done := make(chan struct{})
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		defer close(done)
		inner.ServeHTTP(w, r)
	}))
	t.Cleanup(server.Close)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, server.URL+"/v1/chat/completions", strings.NewReader(userHello))
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer "+testKey)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if _, err = bufio.NewReader(resp.Body).ReadString('\n'); err != nil {
		t.Fatal(err)
	}
	cancel()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("the delay ignored the client cancel")
	}
}

func TestModelsListShape(t *testing.T) {
	t.Parallel()
	script := mustParse(t, `{"models":[{"id":"qwen-test","context_length":32000,"architecture":{"input_modalities":["text","image"]}}],"turns":[{"id":"t1","when":{"user":"hi"},"steps":[{"finish":"stop"}]}]}`)
	h := NewHandler(script, Options{Key: testKey})
	rec := serve(t, h, modelRoute, "Bearer "+testKey, "")
	const want = `{"object":"list","data":[{"id":"qwen-test","context_length":32000,"architecture":{"input_modalities":["text","image"]}}]}`
	if rec.Code != http.StatusOK || rec.Body.String() != want {
		t.Fatalf("status = %d, body = %s, want %s", rec.Code, rec.Body.String(), want)
	}
}

func TestRecordWritesOneLinePerRequest(t *testing.T) {
	t.Parallel()
	script := mustParse(t, `{`+modelsJSON+`,"turns":[{"id":"t1","when":{"user":"hello"},"steps":[{"finish":"stop"}]}]}`)
	var record, log bytes.Buffer
	h := NewHandler(script, Options{Key: testKey, Record: &record, Log: &log})
	body := "{\n \"messages\": [{\"role\": \"user\", \"content\": \"hello\"}]\n}"
	serve(t, h, chatRoute, "Bearer "+testKey, body)
	want := `{"seq":1,"path":"/v1/chat/completions","turn":"t1","body":{"messages":[{"role":"user","content":"hello"}]}}` + "\n"
	if record.String() != want {
		t.Fatalf("record = %q, want %q", record.String(), want)
	}
	if log.String() != "llmstub: 1 t1 /v1/chat/completions\n" {
		t.Fatalf("log = %q", log.String())
	}
}

func TestParseRejectsUnknownInputModality(t *testing.T) {
	t.Parallel()
	expectParseError(t, `{"models":[{"id":"qwen-test","architecture":{"input_modalities":["text","audio"]}}],"turns":[{"id":"t1","when":{"user":"hi"},"steps":[{"text":"a"},{"finish":"stop"}]}]}`, `input_modalities entry "audio" must be text or image`)
}

func TestParseRejectsExplicitZeroChunks(t *testing.T) {
	t.Parallel()
	expectParseError(t, `{"models":[{"id":"qwen-test"}],"turns":[{"id":"t1","when":{"user":"hi"},"steps":[{"tool_call":{"id":"c1","name":"read","arguments":{"path":"a"},"chunks":0}},{"finish":"tool_calls"}]}]}`, "chunks must be 1 to 16")
}

func TestParseDefaultsMissingChunksToOne(t *testing.T) {
	t.Parallel()
	script := mustParse(t, `{"models":[{"id":"qwen-test"}],"turns":[{"id":"t1","when":{"user":"hi"},"steps":[{"tool_call":{"id":"c1","name":"read","arguments":{"path":"a"}}},{"finish":"tool_calls"}]}]}`)
	if got := script.Turns[0].Steps[0].ToolCall.Chunks; got != 1 {
		t.Fatalf("chunks = %d, want 1", got)
	}
}

func TestConcurrentStreamErrorsShareOneLogWriter(t *testing.T) {
	t.Parallel()
	script := mustParse(t, `{"models":[{"id":"qwen-test"}],"turns":[{"id":"slow","when":{"user":"hello"},"steps":[{"text":"start"},{"delay_ms":60000},{"finish":"stop"}]}]}`)
	var logs bytes.Buffer
	server := httptest.NewServer(NewHandler(script, Options{Key: testKey, Log: &logs}))
	var wg sync.WaitGroup
	for range 32 {
		wg.Go(func() { streamUntilStart(t, server.URL) })
	}
	wg.Wait()
	server.Close()
	if got := strings.Count(logs.String(), " stream: "); got != 32 {
		t.Fatalf("logged %d stream errors, want 32:\n%s", got, logs.String())
	}
}

func streamUntilStart(t *testing.T, url string) {
	t.Helper()
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url+"/v1/chat/completions", strings.NewReader(userHello))
	if err != nil {
		t.Error(err)
		return
	}
	req.Header.Set("Authorization", "Bearer "+testKey)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Error(err)
		return
	}
	defer func() { _ = resp.Body.Close() }()
	reader := bufio.NewReader(resp.Body)
	for {
		line, err := reader.ReadString('\n')
		if strings.Contains(line, `"content":"start"`) {
			return
		}
		if err != nil {
			t.Error(err)
			return
		}
	}
}

func TestLiveEditReviewContinuationKeepsJudgeRouting(t *testing.T) {
	t.Parallel()
	raw, err := os.ReadFile("testdata/live.json")
	if err != nil {
		t.Fatal(err)
	}
	script := mustParse(t, string(raw))
	h := &handler{script: script, spent: make(map[string]bool)}
	feedback := `<edit-review call="call-patch" paths="notes.md">`
	for _, tc := range []struct {
		last  lastMessage
		texts []string
		want  string
	}{
		{lastMessage{Role: "user", Text: feedback}, []string{feedback}, "t-edit-review-continue"},
		{lastMessage{Role: "user", Text: `{"off_task":0}`}, []string{feedback, `{"off_task":0}`}, "t-judge"},
		{lastMessage{Role: "user", Text: `{"incomplete":0}`}, []string{feedback, `{"incomplete":0}`}, "t-judge-turn"},
	} {
		turn, ok := h.pick(requestView{Last: tc.last, Texts: tc.texts})
		if !ok || turn.ID != tc.want {
			t.Fatalf("picked %q, matched %t, want %q", turn.ID, ok, tc.want)
		}
		if len(turn.Steps) != 2 || turn.Steps[1].Finish != "stop" {
			t.Fatalf("%s does not end with a normal reply: %+v", turn.ID, turn.Steps)
		}
	}
	if turn, ok := h.pick(requestView{Last: lastMessage{Role: "user", Text: "unrelated feedback"}}); ok {
		t.Fatalf("unrelated feedback matched %s", turn.ID)
	}
}

func TestLiveRatchetRequestsDirectApproval(t *testing.T) {
	t.Parallel()
	raw, err := os.ReadFile("testdata/live.json")
	if err != nil {
		t.Fatal(err)
	}
	h := &handler{script: mustParse(t, string(raw)), spent: make(map[string]bool)}
	env := liveRatchetEnv(t)
	asker := &liveRatchetAsker{t: t}
	editor := ratchet.New(asker)
	for _, tc := range []struct {
		last lastMessage
		want string
		tool string
	}{
		{lastMessage{Role: "user", Text: "start a ratchet"}, "t-ratchet", "call-ratchet"},
		{lastMessage{Role: "tool", ToolCallID: "call-ratchet"}, "t-ratchet-2", "call-ratchet-approve"},
		{lastMessage{Role: "tool", ToolCallID: "call-ratchet-approve"}, "t-ratchet-approved", ""},
	} {
		turn, ok := h.pick(requestView{Last: tc.last})
		if !ok || turn.ID != tc.want {
			t.Fatalf("picked %q, matched %t, want %q", turn.ID, ok, tc.want)
		}
		if tc.tool == "" {
			if turn.Steps[len(turn.Steps)-1].Finish != "stop" {
				t.Fatal("approval response does not end the turn")
			}
			continue
		}
		runLiveRatchetCall(t, editor, env, turn, tc.tool)
	}
	if asker.calls != 1 {
		t.Fatalf("approval questions = %d, want 1", asker.calls)
	}
}

func liveRatchetEnv(t *testing.T) tool.Env {
	t.Helper()
	local, err := runner.NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	env := tool.Env{Cwd: t.TempDir(), Runner: local}
	if err := os.CopyFS(env.Cwd, os.DirFS("testdata/workspace")); err != nil {
		t.Fatal(err)
	}
	return env
}

func runLiveRatchetCall(t *testing.T, editor ratchet.Tool, env tool.Env, turn Turn, callID string) {
	t.Helper()
	call := turn.Steps[0].ToolCall
	if call == nil || call.Name != "ratchet" || call.ID != callID {
		t.Fatalf("expected direct ratchet call %s, got %+v", callID, call)
	}
	result, err := editor.Run(t.Context(), tool.Call{ID: call.ID, Name: call.Name, Args: call.Arguments}, env)
	if err != nil || !result.OK {
		t.Fatalf("%s: result %+v, error %v", call.ID, result, err)
	}
	if call.ID == "call-ratchet-approve" && !bytes.Contains(result.Output, []byte(`"approved":true`)) {
		t.Fatalf("approval result = %s", result.Output)
	}
}

type liveRatchetAsker struct {
	t     *testing.T
	calls int
}

func (a *liveRatchetAsker) Ask(_ context.Context, _ string, q ratchet.Question) (ratchet.Answer, error) {
	a.calls++
	if len(q.Options) != 3 || q.Options[0].Label != "Approve" || q.Options[1].Label != "Revise" || q.Options[2].Label != "Abort" {
		a.t.Fatalf("approval options = %+v", q.Options)
	}
	return ratchet.Answer{Label: "Approve"}, nil
}
