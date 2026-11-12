package devin_test

import (
	"bytes"
	"compress/gzip"
	"context"
	"encoding/base64"
	"io"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"github.com/gosuda/dalgona/internal/model/devin"
	"github.com/gosuda/dalgona/internal/model/devin/wire"
)

const sessionPrefix = "devin-session-token$"

type chatSeen struct {
	apiKey, jwt, system, model, cascade string
	requestType                         int32
	prompts                             []wire.ChatMessagePrompt
	tools                               []wire.ChatToolDefinition
}

type fakeDevin struct {
	t            *testing.T
	mu           sync.Mutex
	exchanges    int
	exchangeKeys []string
	rejectPrefix bool
	expiresIn    time.Duration
	chats        []chatSeen
	chatReplies  []func(http.ResponseWriter)
}

func (f *fakeDevin) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		f.t.Errorf("read body: %v", err)
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	switch r.URL.Path {
	case "/exa.auth_pb.AuthService/GetUserJwt":
		f.serveJWT(w, body)
	case "/exa.api_server_pb.ApiServerService/GetChatMessage":
		f.serveChat(w, r, body)
	case "/exa.api_server_pb.ApiServerService/GetCliModelConfigs":
		serveModels(w)
	default:
		http.NotFound(w, r)
	}
}

func (f *fakeDevin) serveJWT(w http.ResponseWriter, body []byte) {
	var request wire.GetUserJWTRequest
	if err := wire.ConsumeGetUserJWTRequest(body, &request); err != nil || request.Metadata == nil {
		f.t.Errorf("decode jwt request: %v", err)
		return
	}
	key := string(request.Metadata.APIKeyView)
	f.exchangeKeys = append(f.exchangeKeys, key)
	if f.rejectPrefix && strings.HasPrefix(key, sessionPrefix) {
		w.WriteHeader(http.StatusUnauthorized)
		_, _ = io.WriteString(w, `{"code":"unauthenticated","message":"bad key"}`)
		return
	}
	f.exchanges++
	_, _ = w.Write(wire.AppendGetUserJWTResponse(nil, &wire.GetUserJWTResponse{UserJWTView: []byte(testJWT(f.exchanges, f.expiresIn))}))
}

func testJWT(serial int, expiresIn time.Duration) string {
	claims := `{"sub":"jwt-` + strconv.Itoa(serial) + `","exp":` + strconv.FormatInt(time.Now().Add(expiresIn).Unix(), 10) + `}`
	return "e30." + base64.RawURLEncoding.EncodeToString([]byte(claims)) + ".sig"
}

func (f *fakeDevin) serveChat(w http.ResponseWriter, r *http.Request, body []byte) {
	if r.Header.Get("Content-Type") != "application/connect+proto" || r.Header.Get("Connect-Content-Encoding") != "gzip" {
		f.t.Errorf("chat headers = %v", r.Header)
	}
	request := decodeChatRequest(f.t, body)
	f.chats = append(f.chats, chatSeen{
		apiKey: string(request.Metadata.APIKeyView), jwt: string(request.Metadata.UserJWTView),
		system: string(request.PromptView), model: string(request.ChatModelUIDView), cascade: string(request.ConversationIDView),
		requestType: request.RequestType, prompts: request.Prompts, tools: request.Tools,
	})
	reply := f.chatReplies[0]
	if len(f.chatReplies) > 1 {
		f.chatReplies = f.chatReplies[1:]
	}
	w.Header().Set("Content-Type", "application/connect+proto")
	reply(w)
}

func decodeChatRequest(t *testing.T, body []byte) *wire.GetChatMessageRequest {
	t.Helper()
	frame, err := connectwire.ReadFrame(bytes.NewReader(body), nil, 1<<24)
	if err != nil || frame.Flags != connectwire.Compressed {
		t.Fatalf("chat frame flags = %d, err = %v", frame.Flags, err)
	}
	reader, err := gzip.NewReader(bytes.NewReader(frame.Payload))
	if err != nil {
		t.Fatal(err)
	}
	raw, err := io.ReadAll(reader)
	if err != nil {
		t.Fatal(err)
	}
	var request wire.GetChatMessageRequest
	if err := wire.ConsumeGetChatMessageRequest(raw, &request); err != nil || request.Metadata == nil {
		t.Fatalf("decode chat request: %v", err)
	}
	return &request
}

func serveModels(w http.ResponseWriter) {
	response := wire.GetCliModelConfigsResponse{Models: []wire.ClientModelConfig{
		{ModelUIDView: []byte("swe-1-6"), LabelView: []byte("SWE-1.6"), ContextWindow: 200000, SupportsImages: true, ModelInfo: &wire.ModelInfo{MaxOutputTokens: 64000}},
		{ModelUIDView: []byte("router"), ModelInfo: &wire.ModelInfo{ModelRouter: true}},
		{ModelUIDView: []byte("old"), Disabled: true},
		{ModelUIDView: []byte("swe-1-6"), ContextWindow: 1},
		{ModelUIDView: []byte("swe-1-6-fast")},
	}}
	_, _ = w.Write(wire.AppendModelConfigResponse(nil, &response))
}

func frame(message *wire.GetChatMessageResponse) []byte {
	return connectwire.AppendFrame(nil, 0, wire.AppendGetChatMessageResponse(nil, message))
}

func gzipFrame(message *wire.GetChatMessageResponse) []byte {
	var buffer bytes.Buffer
	writer := gzip.NewWriter(&buffer)
	_, _ = writer.Write(wire.AppendGetChatMessageResponse(nil, message))
	_ = writer.Close()
	return connectwire.AppendFrame(nil, connectwire.Compressed, buffer.Bytes())
}

func endFrame(trailer string) []byte {
	return connectwire.AppendFrame(nil, connectwire.EndStream, []byte(trailer))
}

func toolTurn(w http.ResponseWriter) {
	var out []byte
	out = append(out, frame(&wire.GetChatMessageResponse{DeltaThinkingView: []byte("plan")})...)
	out = append(out, gzipFrame(&wire.GetChatMessageResponse{DeltaTextView: []byte("hel")})...)
	out = append(out, frame(&wire.GetChatMessageResponse{DeltaTextView: []byte("lo")})...)
	out = append(out, frame(&wire.GetChatMessageResponse{ToolCalls: []wire.ChatToolCall{{IDView: []byte("call_9"), NameView: []byte("read"), ArgumentsView: []byte(`{"path":`)}}})...)
	out = append(out, frame(&wire.GetChatMessageResponse{ToolCalls: []wire.ChatToolCall{{ArgumentsView: []byte(`"a.go"}`)}}})...)
	out = append(out, frame(&wire.GetChatMessageResponse{StopReason: 10, Usage: &wire.ModelUsageStats{Input: 11, Output: 5, CacheRead: 3, CacheWrite: 2}})...)
	out = append(out, endFrame(`{}`)...)
	_, _ = w.Write(out)
}

func textTurn(w http.ResponseWriter) {
	out := frame(&wire.GetChatMessageResponse{DeltaTextView: []byte("ok"), StopReason: 2})
	_, _ = w.Write(append(out, endFrame(`{}`)...))
}

func errorTurn(code string) func(http.ResponseWriter) {
	return func(w http.ResponseWriter) {
		_, _ = w.Write(endFrame(`{"error":{"code":"` + code + `","message":"try later"}}`))
	}
}

func unauthorizedTurn(w http.ResponseWriter) {
	w.WriteHeader(http.StatusUnauthorized)
	_, _ = io.WriteString(w, `{"code":"unauthenticated","message":"expired jwt"}`)
}

func newFake(t *testing.T, replies ...func(http.ResponseWriter)) (*fakeDevin, *httptest.Server) {
	t.Helper()
	fake := &fakeDevin{t: t, expiresIn: time.Hour, chatReplies: replies}
	server := httptest.NewServer(fake)
	t.Cleanup(server.Close)
	return fake, server
}

func newProvider(server *httptest.Server, retries *atomic.Int32) *devin.Provider {
	return devin.New(devin.Config{
		BaseURL: server.URL,
		APIKey:  func(context.Context) (string, error) { return "user-key", nil },
		Retry: model.RetryOptions{
			StartTimeout: 2 * time.Second,
			IdleTimeout:  2 * time.Second,
			Sleep:        func(context.Context, time.Duration) error { return nil },
			OnRetry:      func(int, model.Verdict, int, time.Duration) { retries.Add(1) },
		},
	})
}

type summary struct {
	text, reasoning string
	calls           []model.ToolCall
	usage           model.Usage
	stop            string
	failure         *model.Error
}

func drain(t *testing.T, provider *devin.Provider, req model.Request) summary {
	t.Helper()
	stream, err := provider.Stream(t.Context(), req)
	if err != nil {
		t.Fatal(err)
	}
	var out summary
	for event := range stream.Events() {
		switch value := event.(type) {
		case model.TextDelta:
			out.text += value.Text
		case model.ReasoningDelta:
			out.reasoning += value.Text
		case model.ToolCall:
			out.calls = append(out.calls, value)
		case model.Usage:
			out.usage = value
		case model.Done:
			out.stop = value.StopReason
		case model.Error:
			out.failure = &value
		}
	}
	return out
}

func historyRequest() model.Request {
	return model.Request{
		Model:  "swe-1-6",
		System: "be brief",
		Messages: []model.Message{
			{Role: "user", Text: "read it"},
			{Role: "assistant", ToolCalls: []model.ToolCall{{ID: "call|item.7", Name: "read", Args: []byte(`{"path":"x"}`)}}},
			{Role: "user", ToolResults: []model.ToolResult{{ID: "call|item.7", Output: []byte(`"contents"`)}}, Text: "now summarize"},
		},
		Tools: []model.Tool{{Name: "read", Description: "Read a file", Parameters: []byte(`{"type":"object"}`)}},
	}
}

func TestStreamDecodesEventsAndSendsHistory(t *testing.T) {
	var retries atomic.Int32
	fake, server := newFake(t, toolTurn)
	out := drain(t, newProvider(server, &retries), historyRequest())
	checkToolTurn(t, out)
	fake.mu.Lock()
	defer fake.mu.Unlock()
	checkChat(t, fake.chats[0])
	checkPrompts(t, fake.chats[0].prompts)
}

func checkToolTurn(t *testing.T, out summary) {
	t.Helper()
	checkSummaryText(t, out)
	if len(out.calls) != 1 || out.calls[0].ID != "call_9" || out.calls[0].Name != "read" || string(out.calls[0].Args) != `{"path":"a.go"}` {
		t.Fatalf("calls = %+v", out.calls)
	}
	if out.usage != (model.Usage{Input: 11, Output: 5, CacheRead: 3, CacheWrite: 2}) {
		t.Fatalf("usage = %+v", out.usage)
	}
}

func checkSummaryText(t *testing.T, out summary) {
	t.Helper()
	if out.failure != nil || out.text != "hello" || out.reasoning != "plan" || out.stop != "toolUse" {
		t.Fatalf("summary = %+v", out)
	}
}

func checkChat(t *testing.T, seen chatSeen) {
	t.Helper()
	envelope := [...]string{seen.apiKey, seen.jwt[:4], seen.system, seen.model, strconv.Itoa(int(seen.requestType))}
	if envelope != [...]string{sessionPrefix + "user-key", "e30.", "be brief", "swe-1-6", "5"} || seen.cascade == "" {
		t.Fatalf("chat = %+v", seen)
	}
	if len(seen.tools) != 1 || seen.tools[0].Strict {
		t.Fatalf("tools = %+v", seen.tools)
	}
	if tool := seen.tools[0]; string(tool.NameView)+" "+string(tool.JSONSchemaView) != `read {"type":"object"}` {
		t.Fatalf("tool = %s %s", tool.NameView, tool.JSONSchemaView)
	}
}

func checkPrompts(t *testing.T, prompts []wire.ChatMessagePrompt) {
	t.Helper()
	got := make([]string, len(prompts))
	for index, prompt := range prompts {
		got[index] = strconv.Itoa(int(prompt.Source)) + ":" + string(prompt.PromptView) + ":" + string(prompt.ToolCallIDView)
		for _, call := range prompt.ToolCalls {
			got[index] += ":" + string(call.IDView) + "=" + string(call.ArgumentsView)
		}
	}
	want := []string{"1:read it:", `2:::call_item_7={"path":"x"}`, "4:contents:call_item_7", "1:now summarize:"}
	if strings.Join(got, "|") != strings.Join(want, "|") {
		t.Fatalf("prompts = %q, want %q", got, want)
	}
}

func TestJWTIsCachedThenRenewedAfterExpiryAndRejection(t *testing.T) {
	var retries atomic.Int32
	fake, server := newFake(t, textTurn)
	provider := newProvider(server, &retries)
	request := model.Request{Model: "swe-1-6", Messages: []model.Message{{Role: "user", Text: "hi"}}}
	for range 2 {
		if out := drain(t, provider, request); out.text != "ok" {
			t.Fatalf("summary = %+v", out)
		}
	}
	fake.mu.Lock()
	if fake.exchanges != 1 {
		t.Fatalf("exchanges with a valid jwt = %d, want 1", fake.exchanges)
	}
	fake.chatReplies = []func(http.ResponseWriter){unauthorizedTurn, textTurn}
	fake.expiresIn = 30 * time.Second
	fake.mu.Unlock()
	if out := drain(t, provider, request); out.text != "ok" {
		t.Fatalf("after rejection = %+v", out)
	}
	drain(t, provider, request)
	drain(t, provider, request)
	fake.mu.Lock()
	defer fake.mu.Unlock()
	if fake.exchanges != 4 {
		t.Fatalf("exchanges = %d, want 1 + 1 after rejection + 1 per stream with a jwt inside the expiry margin", fake.exchanges)
	}
}

func TestExchangeFallsBackToRawKey(t *testing.T) {
	var retries atomic.Int32
	fake, server := newFake(t, textTurn)
	fake.rejectPrefix = true
	out := drain(t, newProvider(server, &retries), model.Request{Model: "swe-1-6", Messages: []model.Message{{Role: "user", Text: "hi"}}})
	fake.mu.Lock()
	defer fake.mu.Unlock()
	if out.text != "ok" || strings.Join(fake.exchangeKeys, ",") != sessionPrefix+"user-key,user-key" || fake.chats[0].apiKey != "user-key" {
		t.Fatalf("summary = %+v, exchange keys = %q, chat = %+v", out, fake.exchangeKeys, fake.chats)
	}
}

func TestRetryableTrailerRetries(t *testing.T) {
	var retries atomic.Int32
	_, server := newFake(t, errorTurn("resource_exhausted"), textTurn)
	if out := drain(t, newProvider(server, &retries), historyRequest()); out.text != "ok" || retries.Load() != 1 {
		t.Fatalf("summary = %+v, retries = %d", out, retries.Load())
	}
}

func TestFailedTurnNeverLeaksItsToolCall(t *testing.T) {
	toolThenFail := func(w http.ResponseWriter) {
		out := frame(&wire.GetChatMessageResponse{ToolCalls: []wire.ChatToolCall{{IDView: []byte("c1"), NameView: []byte("read"), ArgumentsView: []byte(`{}`)}}, StopReason: 10})
		_, _ = w.Write(append(out, endFrame(`{"error":{"code":"unavailable","message":"down"}}`)...))
	}
	var retries atomic.Int32
	_, server := newFake(t, toolThenFail, toolTurn)
	out := drain(t, newProvider(server, &retries), historyRequest())
	if out.failure != nil || retries.Load() != 1 || len(out.calls) != 1 || out.calls[0].ID != "call_9" {
		t.Fatalf("failure = %+v, retries = %d, calls = %+v", out.failure, retries.Load(), out.calls)
	}
}

func TestTerminalTrailerFailsWithStatus(t *testing.T) {
	var retries atomic.Int32
	_, server := newFake(t, errorTurn("permission_denied"))
	out := drain(t, newProvider(server, &retries), historyRequest())
	if out.failure == nil || out.failure.Status != http.StatusForbidden || retries.Load() != 0 {
		t.Fatalf("failure = %+v, retries = %d", out.failure, retries.Load())
	}
}

func TestModelsSkipRoutersDisabledAndDuplicates(t *testing.T) {
	var retries atomic.Int32
	_, server := newFake(t, textTurn)
	models, err := newProvider(server, &retries).Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if len(models) != 2 || models[0].ID != "swe-1-6" || models[0].Context != 200000 || models[0].MaxOutput != 64000 || models[1].Context != 128000 {
		t.Fatalf("models = %+v", models)
	}
}

func TestMissingEnvironmentKey(t *testing.T) {
	t.Setenv("DEVIN_API_KEY", "")
	if _, err := devin.APIKeyFromEnv(t.Context()); err == nil || err.Error() != "DEVIN_API_KEY is not set" {
		t.Fatalf("err = %v", err)
	}
}

func TestListedModelCarriesSupportsImages(t *testing.T) {
	var retries atomic.Int32
	_, server := newFake(t, textTurn)
	models, err := newProvider(server, &retries).Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if len(models) != 2 {
		t.Fatalf("models = %d, want 2", len(models))
	}
	if !models[0].Images || models[1].Images {
		t.Fatalf("images = %v %v, want swe-1-6 true and swe-1-6-fast false", models[0].Images, models[1].Images)
	}
}
