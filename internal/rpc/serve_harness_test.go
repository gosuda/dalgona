package rpc

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/journal"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/openai"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/exec"
	"github.com/gosuda/dalgona/internal/tool/read"
	"github.com/santhosh-tekuri/jsonschema/v6"
)

const waitLimit = 20 * time.Second

func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == "__shim" {
		os.Exit(runner.ShimMain(os.Args[2:]))
	}
	os.Exit(m.Run())
}

type llmReply struct {
	body string
	gate <-chan struct{}
}

type fakeLLM struct {
	server *httptest.Server
	mu     sync.Mutex
	queue  []llmReply
	count  int
}

func newFakeLLM(t *testing.T) *fakeLLM {
	t.Helper()
	f := &fakeLLM{}
	mux := http.NewServeMux()
	mux.HandleFunc("POST /responses", f.respond)
	mux.HandleFunc("GET /models", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, `{"data":[{"id":"gpt-test"}]}`)
	})
	f.server = httptest.NewServer(mux)
	t.Cleanup(f.server.Close)
	return f
}

func (f *fakeLLM) push(replies ...llmReply) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.queue = append(f.queue, replies...)
}

func (f *fakeLLM) next() llmReply {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.count++
	if len(f.queue) == 0 {
		return llmReply{body: sseText("done")}
	}
	reply := f.queue[0]
	f.queue = f.queue[1:]
	return reply
}

func (f *fakeLLM) respond(w http.ResponseWriter, r *http.Request) {
	if _, err := io.Copy(io.Discard, r.Body); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	reply := f.next()
	if reply.gate != nil {
		select {
		case <-reply.gate:
		case <-r.Context().Done():
			return
		}
	}
	w.Header().Set("Content-Type", "text/event-stream")
	_, _ = io.WriteString(w, reply.body)
}

func sseEvent(kind, data string) string { return "event: " + kind + "\ndata: " + data + "\n\n" }

func sseCompleted() string {
	return sseEvent("response.completed", `{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":20,"output_tokens":7}}}`)
}

func quoted(s string) string { return strconv.Quote(s) }

func sseText(parts ...string) string {
	var b strings.Builder
	for _, part := range parts {
		b.WriteString(sseEvent("response.output_text.delta", `{"type":"response.output_text.delta","output_index":0,"delta":`+quoted(part)+`}`))
	}
	b.WriteString(sseCompleted())
	return b.String()
}

func sseCall(id, name, args string) string {
	head := `{"type":"function_call","call_id":"` + id + `","name":"` + name + `","arguments":`
	return sseEvent("response.output_item.added", `{"type":"response.output_item.added","output_index":1,"item":`+head+`""}}`) +
		sseEvent("response.output_item.done", `{"type":"response.output_item.done","output_index":1,"item":`+head+quoted(args)+`}}`) +
		sseCompleted()
}

type rig struct {
	t    *testing.T
	home string
	cwd  string
	llm  *fakeLLM
	host *agent.Host
	cfg  agent.HostConfig
}

func newRig(t *testing.T, approval config.Approval) *rig {
	t.Helper()
	r := &rig{t: t, home: t.TempDir(), cwd: t.TempDir(), llm: newFakeLLM(t)}
	t.Setenv("DAL_HOME", r.home)
	if resolved, err := filepath.EvalSymlinks(r.cwd); err == nil {
		r.cwd = resolved
	}
	store, err := journal.Open(r.home)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	credentials, err := auth.New(r.home)
	if err != nil {
		t.Fatal(err)
	}
	r.cfg = agent.HostConfig{
		Journal: store, Auth: credentials, ProviderIDs: []string{"openai"},
		Config: config.Config{
			Home: r.home, Cwd: r.cwd, Model: "openai/gpt-test", Approval: approval, Sandbox: config.SandboxOff,
			Product: config.ProductDal, Reasoning: config.ReasoningDefault,
		},
		Providers: r.providers,
		Tools: func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{read.New(), exec.New()}, nil
		},
		Runner: func(_ context.Context, id, _ string) (runner.Runner, error) {
			return runner.NewLocal(filepath.Join(r.home, "sessions", id, "runs"))
		},
		Logger: slog.New(slog.DiscardHandler),
	}
	r.openHost()
	return r
}

func (r *rig) openHost() {
	r.t.Helper()
	host, err := agent.NewHost(r.cfg)
	if err != nil {
		r.t.Fatal(err)
	}
	r.t.Cleanup(func() { _ = host.Close() })
	r.host = host
}

func (r *rig) providers(onRetry agent.RetryFunc) (*model.Registry, error) {
	registry := model.NewRegistry()
	retry := model.DefaultRetryOptions()
	retry.Sleep = func(context.Context, time.Duration) error { return nil }
	retry.OnRetry = onRetry
	provider := openai.New(openai.Config{
		BaseURL: r.llm.server.URL, Client: r.llm.server.Client(), Retry: retry,
		APIKey: func(context.Context) (string, error) { return "test-key", nil },
	})
	return registry, registry.Register("openai", provider)
}

type message struct {
	ID     jsontext.Value `json:"id,omitzero"`
	Method string         `json:"method,omitzero"`
	Params jsontext.Value `json:"params,omitzero"`
	Result jsontext.Value `json:"result,omitzero"`
	Error  *RPCError      `json:"error,omitzero"`
}

type wireClient struct {
	t        *testing.T
	conn     net.Conn
	writeMu  sync.Mutex
	mu       sync.Mutex
	next     int
	waiting  map[string]chan message
	methods  map[string]string
	notes    chan message
	requests chan message
	served   chan error
}

func (r *rig) connect() *wireClient { return r.connectWith(ServeOptions{}) }

func (r *rig) connectWith(opts ServeOptions) *wireClient {
	r.t.Helper()
	left, right := net.Pipe()
	c := &wireClient{
		t: r.t, conn: right, waiting: map[string]chan message{}, methods: map[string]string{},
		notes: make(chan message, 100000), requests: make(chan message, 64), served: make(chan error, 1),
	}
	go func() { c.served <- ServeWith(r.t.Context(), left, r.host, opts) }()
	go c.read()
	r.t.Cleanup(c.close)
	return c
}

func (c *wireClient) close() {
	_ = c.conn.Close()
	select {
	case err := <-c.served:
		if err != nil {
			c.t.Errorf("Serve returned %v", err)
		}
	case <-time.After(waitLimit):
		c.t.Error("Serve did not return after the client closed")
	}
}

func (c *wireClient) read() {
	scanner := bufio.NewScanner(c.conn)
	scanner.Buffer(make([]byte, 64<<10), 16<<20)
	for scanner.Scan() {
		line := bytes.Clone(scanner.Bytes())
		var m message
		if err := json.Unmarshal(line, &m); err != nil {
			c.t.Errorf("undecodable line %s: %v", line, err)
			continue
		}
		c.route(line, m)
	}
}

func (c *wireClient) route(line []byte, m message) {
	switch {
	case m.Method != "" && len(m.ID) > 0:
		validateLine(c.t, "ServerRequest.json", line)
		c.requests <- m
	case m.Method != "":
		if !strings.HasPrefix(m.Method, "dalgona/") {
			validateLine(c.t, "ServerNotification.json", line)
		}
		c.notes <- m
	default:
		c.mu.Lock()
		ch := c.waiting[string(m.ID)]
		method := c.methods[string(m.ID)]
		c.mu.Unlock()
		c.checkResponse(method, m)
		if ch != nil {
			ch <- m
		}
	}
}

var responseSchemas = map[string]string{
	"initialize":           "v1/InitializeResponse.json",
	"thread/start":         "v2/ThreadStartResponse.json",
	"thread/resume":        "v2/ThreadResumeResponse.json",
	"thread/fork":          "v2/ThreadForkResponse.json",
	"thread/list":          "v2/ThreadListResponse.json",
	"thread/read":          "v2/ThreadReadResponse.json",
	"thread/name/set":      "v2/ThreadSetNameResponse.json",
	"thread/archive":       "v2/ThreadArchiveResponse.json",
	"thread/unarchive":     "v2/ThreadUnarchiveResponse.json",
	"thread/compact/start": "v2/ThreadCompactStartResponse.json",
	"thread/revert":        "v2/ThreadRevertResponse.json",
	"turn/start":           "v2/TurnStartResponse.json",
	"turn/steer":           "v2/TurnSteerResponse.json",
	"turn/interrupt":       "v2/TurnInterruptResponse.json",
	"model/list":           "v2/ModelListResponse.json",
	"config/read":          "v2/ConfigReadResponse.json",
	"config/value/write":   "v2/ConfigWriteResponse.json",
	"account/read":         "v2/GetAccountResponse.json",
	"account/login/start":  "v2/LoginAccountResponse.json",
	"account/login/cancel": "v2/CancelLoginAccountResponse.json",
	"account/logout":       "v2/LogoutAccountResponse.json",
	"skills/list":          "v2/SkillsListResponse.json",
}

var (
	schemaMu    sync.Mutex
	schemaCache = map[string]*jsonschema.Schema{}
)

func validateLine(t *testing.T, path string, raw []byte) {
	t.Helper()
	schemaMu.Lock()
	sch, ok := schemaCache[path]
	if !ok {
		var err error
		sch, err = jsonschema.NewCompiler().Compile(filepath.Join("testdata", "codex-schema", path))
		if err != nil {
			schemaMu.Unlock()
			t.Errorf("compile %s: %v", path, err)
			return
		}
		schemaCache[path] = sch
	}
	schemaMu.Unlock()
	value, err := jsonschema.UnmarshalJSON(strings.NewReader(string(raw)))
	if err != nil {
		t.Errorf("decode %s: %v", raw, err)
		return
	}
	if err := sch.Validate(value); err != nil {
		t.Errorf("%s does not validate against %s: %v", raw, path, err)
	}
}

func (c *wireClient) send(v any) {
	line, err := json.Marshal(v)
	if err != nil {
		c.t.Fatal(err)
	}
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	if _, err := c.conn.Write(append(line, '\n')); err != nil {
		c.t.Fatal(err)
	}
}

func (c *wireClient) call(method string, params any) message {
	c.t.Helper()
	c.mu.Lock()
	c.next++
	id := strconv.Itoa(c.next)
	ch := make(chan message, 1)
	c.waiting[id] = ch
	c.methods[id] = method
	c.mu.Unlock()
	raw, err := json.Marshal(params)
	if err != nil {
		c.t.Fatal(err)
	}
	c.send(message{ID: jsontext.Value(id), Method: method, Params: raw})
	select {
	case m := <-ch:
		return m
	case <-time.After(waitLimit):
		c.t.Fatalf("no response to %s", method)
		return message{}
	}
}

func (c *wireClient) ok(method string, params any, out any) {
	c.t.Helper()
	m := c.call(method, params)
	if m.Error != nil {
		c.t.Fatalf("%s failed: %d %s", method, m.Error.Code, m.Error.Message)
	}
	if out != nil {
		if err := json.Unmarshal(m.Result, out); err != nil {
			c.t.Fatalf("decode %s result %s: %v", method, m.Result, err)
		}
	}
}

func (c *wireClient) fails(method string, params any, code int, text string) {
	c.t.Helper()
	m := c.call(method, params)
	if m.Error == nil || m.Error.Code != code || m.Error.Message != text {
		c.t.Fatalf("%s: got error %+v result %s, want %d %q", method, m.Error, m.Result, code, text)
	}
}

func (c *wireClient) await(method string, match func(message) bool) message {
	c.t.Helper()
	deadline := time.After(waitLimit)
	for {
		select {
		case m := <-c.notes:
			if m.Method == method && (match == nil || match(m)) {
				return m
			}
		case <-deadline:
			c.t.Fatalf("no %s notification", method)
			return message{}
		}
	}
}

func (c *wireClient) collect(until string) []message {
	c.t.Helper()
	var seen []message
	deadline := time.After(waitLimit)
	for {
		select {
		case m := <-c.notes:
			seen = append(seen, m)
			if m.Method == until {
				return seen
			}
		case <-deadline:
			c.t.Fatalf("no %s notification after %d notifications", until, len(seen))
			return nil
		}
	}
}

func (c *wireClient) respond(id jsontext.Value, result string) {
	c.send(message{ID: id, Result: jsontext.Value(result)})
}

func itemOf(t *testing.T, m message) Item {
	t.Helper()
	var envelope struct {
		Item Item `json:"item"`
	}
	if err := json.Unmarshal(m.Params, &envelope); err != nil {
		t.Fatalf("decode item %s: %v", m.Params, err)
	}
	return envelope.Item
}

func fieldOf(t *testing.T, raw jsontext.Value, path ...string) string {
	t.Helper()
	var value any
	if err := json.Unmarshal(raw, &value); err != nil {
		t.Fatal(err)
	}
	for _, key := range path {
		object, ok := value.(map[string]any)
		if !ok {
			return ""
		}
		value = object[key]
	}
	return fmt.Sprint(value)
}

func (c *wireClient) checkResponse(method string, m message) {
	if m.Error != nil || strings.HasPrefix(method, "dalgona/") {
		return
	}
	schema, ok := responseSchemas[method]
	if !ok {
		c.t.Errorf("no vendored schema mapped for %s response", method)
		return
	}
	validateLine(c.t, schema, m.Result)
}

func effectiveWorkspaceWriteType() string {
	if runner.Available() {
		return "workspaceWrite"
	}
	return "dangerFullAccess"
}
