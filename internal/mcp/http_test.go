package mcp_test

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"

	"github.com/gosuda/dalgona/internal/mcp"
	"github.com/gosuda/dalgona/internal/tool"
)

const (
	initResult     = `{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"http-test","version":"1"}}`
	echoListResult = `{"tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object"}}]}`
	sseListResult  = `{"tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object","x-http-schema":true}}]}`
)

func TestHTTPJSONAndSSEWithSession(t *testing.T) {
	t.Parallel()
	server := newSSEServer(t)
	client, err := mcp.New(t.Context(), mcp.Config{Name: "web", URL: server.URL})
	if err != nil {
		t.Fatalf("connect HTTP client: %v", err)
	}
	t.Cleanup(func() {
		if closeErr := client.Close(); closeErr != nil {
			t.Errorf("close client: %v", closeErr)
		}
	})
	tools, err := client.Tools(t.Context())
	if err != nil {
		t.Fatalf("list tools over SSE: %v", err)
	}
	if len(tools) != 1 || tools[0].Spec().Name != "web__echo" {
		t.Fatalf("unexpected tools: %#v", tools)
	}
	if !strings.Contains(string(tools[0].Spec().Schema), `"x-http-schema":true`) {
		t.Fatalf("schema did not pass through: %s", tools[0].Spec().Schema)
	}
	assertCall(t, tools[0], `{}`, true, "http ok")
}

type sseServer struct {
	t           *testing.T
	initialized atomic.Bool
}

func newSSEServer(t *testing.T) *httptest.Server {
	server := &sseServer{t: t}
	mux := http.NewServeMux()
	mux.HandleFunc("POST /", server.handle)
	httpServer := httptest.NewServer(mux)
	t.Cleanup(httpServer.Close)
	return httpServer
}

func (s *sseServer) handle(writer http.ResponseWriter, request *http.Request) {
	if got := request.Header.Get("Accept"); got != "application/json, text/event-stream" {
		s.t.Errorf("Accept = %q", got)
	}
	message, err := decodeRequest(request.Body)
	if err != nil {
		s.t.Errorf("decode request: %v", err)
		writer.WriteHeader(http.StatusBadRequest)
		return
	}
	switch message.Method {
	case "initialize":
		writer.Header().Set("Mcp-Session-Id", "session-123")
		writeHTTPJSON(s.t, writer, message.ID, initResult)
	case "notifications/initialized":
		s.initialized.Store(true)
		writer.WriteHeader(http.StatusAccepted)
	case "tools/list":
		s.list(writer, request, message)
	case "tools/call":
		s.call(writer, request, message)
	default:
		s.t.Errorf("unexpected method %s", message.Method)
		writer.WriteHeader(http.StatusBadRequest)
	}
}

func (s *sseServer) list(writer http.ResponseWriter, request *http.Request, message testRequest) {
	if !s.initialized.Load() {
		s.t.Error("tools/list arrived before initialized notification")
	}
	s.requireSessionHeaders(request, message)
	result := encodeResponse(s.t, message.ID, sseListResult)
	split := bytes.Index(result, []byte(`,"result":`))
	if split < 0 {
		s.t.Error("test response has no result member")
		writer.WriteHeader(http.StatusInternalServerError)
		return
	}
	writer.Header().Set("Content-Type", "text/event-stream")
	_, _ = fmt.Fprint(writer, ": prime\r\n\r\n")
	_, _ = fmt.Fprintf(writer, "data: %s\r\n\r\n", encodeResponse(s.t, jsontext.Value("999"), `{}`))
	_, _ = fmt.Fprint(writer, "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{}}\r\n\r\n")
	_, _ = fmt.Fprintf(writer, "id: event-1\r\ndata: %s\r\ndata: %s\r\n\r\n", result[:split], result[split:])
}

func (s *sseServer) call(writer http.ResponseWriter, request *http.Request, message testRequest) {
	s.requireSessionHeaders(request, message)
	writeHTTPJSON(s.t, writer, message.ID, `{"content":[{"type":"text","text":"http ok"}],"isError":false}`)
}

func (s *sseServer) requireSessionHeaders(request *http.Request, message testRequest) {
	if request.Header.Get("Mcp-Session-Id") != "session-123" {
		s.t.Errorf("missing session id for %s", message.Method)
	}
	if got := request.Header.Get("MCP-Protocol-Version"); got != "2025-11-25" {
		s.t.Errorf("protocol version = %q", got)
	}
}

func TestHTTPSessionExpiryReinitializes(t *testing.T) {
	t.Parallel()
	fixture := &jsonServer{
		listBody:        echoListResult,
		callBody:        `{"content":[{"type":"text","text":"after expiry"}],"isError":false}`,
		expireFirstCall: true,
	}
	server := newJSONServer(t, fixture)
	client, err := mcp.New(t.Context(), mcp.Config{Name: "web", URL: server.URL})
	if err != nil {
		t.Fatalf("connect HTTP client: %v", err)
	}
	t.Cleanup(func() { _ = client.Close() })
	tools, err := client.Tools(t.Context())
	if err != nil {
		t.Fatalf("list tools: %v", err)
	}
	result, err := tools[0].Run(t.Context(), tool.Call{Args: jsontext.Value(`{}`)}, tool.Env{})
	if err != nil {
		t.Fatalf("call after session expiry: %v", err)
	}
	if !result.OK || string(result.Output) != "after expiry" {
		t.Fatalf("call = (%v, %q), want (true, after expiry)", result.OK, result.Output)
	}
	fixture.mu.Lock()
	defer fixture.mu.Unlock()
	if fixture.sessions != 2 {
		t.Fatalf("initialize count = %d, want 2", fixture.sessions)
	}
	for i, header := range fixture.initializeHeaders {
		if header != "" {
			t.Fatalf("initialize %d carried session header %q", i+1, header)
		}
	}
}

func TestHTTPCloseDeletesSession(t *testing.T) {
	t.Parallel()
	fixture := &jsonServer{listBody: echoListResult}
	server := newJSONServer(t, fixture)
	client, err := mcp.New(t.Context(), mcp.Config{Name: "web", URL: server.URL})
	if err != nil {
		t.Fatalf("connect HTTP client: %v", err)
	}
	if _, listErr := client.Tools(t.Context()); listErr != nil {
		t.Fatalf("list tools: %v", listErr)
	}
	if err := client.Close(); err != nil {
		t.Fatalf("close client: %v", err)
	}
	fixture.mu.Lock()
	defer fixture.mu.Unlock()
	if fixture.deletedSession != fixture.lastSession {
		t.Fatalf("close sent DELETE with session id %q, want %q", fixture.deletedSession, fixture.lastSession)
	}
}

func TestHTTPRepeatedCursorFails(t *testing.T) {
	t.Parallel()
	fixture := &jsonServer{listBody: `{"tools":[],"nextCursor":"loop"}`}
	server := newJSONServer(t, fixture)
	client, err := mcp.New(t.Context(), mcp.Config{Name: "web", URL: server.URL})
	if err != nil {
		t.Fatalf("connect HTTP client: %v", err)
	}
	t.Cleanup(func() { _ = client.Close() })
	_, toolsErr := client.Tools(t.Context())
	if toolsErr == nil {
		t.Fatal("listing tools with a looping cursor succeeded")
	}
	if !strings.Contains(toolsErr.Error(), "repeated cursor") {
		t.Fatalf("cursor loop error = %v", toolsErr)
	}
}

type jsonServer struct {
	t *testing.T

	mu                sync.Mutex
	sessions          int
	initializeHeaders []string
	lastSession       string
	callCount         int
	expired           bool
	deletedSession    string
	listBody          string
	callBody          string
	expireFirstCall   bool
}

func newJSONServer(t *testing.T, fixture *jsonServer) *httptest.Server {
	fixture.t = t
	mux := http.NewServeMux()
	mux.HandleFunc("POST /", fixture.handle)
	mux.HandleFunc("DELETE /", fixture.handleDelete)
	httpServer := httptest.NewServer(mux)
	t.Cleanup(httpServer.Close)
	return httpServer
}

func (s *jsonServer) handle(writer http.ResponseWriter, request *http.Request) {
	message, err := decodeRequest(request.Body)
	if err != nil {
		writer.WriteHeader(http.StatusBadRequest)
		return
	}
	switch message.Method {
	case "initialize":
		s.initialize(writer, request, message)
	case "notifications/initialized":
		writer.WriteHeader(http.StatusAccepted)
	case "tools/list":
		writeHTTPJSON(s.t, writer, message.ID, s.listBody)
	case "tools/call":
		s.call(writer, message)
	default:
		writer.WriteHeader(http.StatusBadRequest)
	}
}

func (s *jsonServer) initialize(writer http.ResponseWriter, request *http.Request, message testRequest) {
	session := s.recordInitialize(request)
	writer.Header().Set("Mcp-Session-Id", session)
	writeHTTPJSON(s.t, writer, message.ID, initResult)
}

func (s *jsonServer) recordInitialize(request *http.Request) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.sessions++
	s.lastSession = fmt.Sprintf("session-%d", s.sessions)
	s.initializeHeaders = append(s.initializeHeaders, request.Header.Get("Mcp-Session-Id"))
	return s.lastSession
}

func (s *jsonServer) call(writer http.ResponseWriter, message testRequest) {
	if s.takeCall() {
		writer.WriteHeader(http.StatusNotFound)
		return
	}
	writeHTTPJSON(s.t, writer, message.ID, s.callBody)
}

func (s *jsonServer) takeCall() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	first := s.callCount == 0
	s.callCount++
	if s.expireFirstCall && first && !s.expired {
		s.expired = true
		return true
	}
	return false
}

func (s *jsonServer) handleDelete(writer http.ResponseWriter, request *http.Request) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.deletedSession = request.Header.Get("Mcp-Session-Id")
	writer.WriteHeader(http.StatusOK)
}

type testRequest struct {
	ID     jsontext.Value
	Method string
}

func decodeRequest(reader io.Reader) (testRequest, error) {
	raw, err := io.ReadAll(io.LimitReader(reader, 1<<20))
	if err != nil {
		return testRequest{}, err
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil || start.Kind() != jsontext.KindBeginObject {
		return testRequest{}, errors.New("request is not an object")
	}
	request := testRequest{}
	for decoder.PeekKind() != jsontext.KindEndObject {
		fieldName, value, memberErr := nextMember(decoder, raw)
		if memberErr != nil {
			return testRequest{}, memberErr
		}
		if applyErr := applyMember(&request, fieldName, value); applyErr != nil {
			return testRequest{}, applyErr
		}
	}
	return request, nil
}

func nextMember(decoder *jsontext.Decoder, raw []byte) (string, jsontext.Value, error) {
	name, err := decoder.ReadToken()
	if err != nil {
		return "", nil, err
	}
	fieldName := name.String()
	value, err := decoder.ReadValue()
	if err != nil {
		return "", nil, err
	}
	end := decoder.InputOffset()
	return fieldName, raw[end-int64(len(value)) : end], nil
}

func applyMember(request *testRequest, fieldName string, value jsontext.Value) error {
	switch fieldName {
	case "id":
		request.ID = value
	case "method":
		decoder := jsontext.NewDecoder(bytes.NewReader(value))
		token, err := decoder.ReadToken()
		if err != nil {
			return err
		}
		request.Method = token.String()
	}
	return nil
}

func writeHTTPJSON(t *testing.T, writer http.ResponseWriter, id jsontext.Value, result string) {
	t.Helper()
	writer.Header().Set("Content-Type", "application/json")
	_, _ = writer.Write(encodeResponse(t, id, result))
}

func encodeResponse(t *testing.T, id jsontext.Value, result string) []byte {
	t.Helper()
	var out bytes.Buffer
	encoder := jsontext.NewEncoder(&out)
	write := func(token jsontext.Token) {
		if err := encoder.WriteToken(token); err != nil {
			t.Errorf("encode response: %v", err)
		}
	}
	write(jsontext.BeginObject)
	write(jsontext.String("jsonrpc"))
	write(jsontext.String("2.0"))
	write(jsontext.String("id"))
	if err := encoder.WriteValue(id); err != nil {
		t.Errorf("encode id: %v", err)
	}
	write(jsontext.String("result"))
	if err := encoder.WriteValue(jsontext.Value(result)); err != nil {
		t.Errorf("encode result: %v", err)
	}
	write(jsontext.EndObject)
	return out.Bytes()
}
