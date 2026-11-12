package llmstub

import (
	"bufio"
	"bytes"
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"
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
