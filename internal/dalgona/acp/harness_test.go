package acp

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"io"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/santhosh-tekuri/jsonschema/v6"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	schemaURL   = "https://dalgona.test/acp/schema.json"
	waitLimit   = 10 * time.Second
	testVersion = "v-test"
)

type validator struct {
	t        *testing.T
	compiler *jsonschema.Compiler
	cache    map[string]*jsonschema.Schema
}

func newValidator(t *testing.T) *validator {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("testdata", "schema", "schema.json"))
	if err != nil {
		t.Fatal(err)
	}
	doc, err := jsonschema.UnmarshalJSON(bytes.NewReader(raw))
	if err != nil {
		t.Fatal(err)
	}
	compiler := jsonschema.NewCompiler()
	if err = compiler.AddResource(schemaURL, doc); err != nil {
		t.Fatal(err)
	}
	return &validator{t: t, compiler: compiler, cache: make(map[string]*jsonschema.Schema)}
}

func (v *validator) check(def string, raw []byte) error {
	v.t.Helper()
	schema, ok := v.cache[def]
	if !ok {
		var err error
		schema, err = v.compiler.Compile(schemaURL + "#/$defs/" + def)
		if err != nil {
			v.t.Fatalf("compile %s: %v", def, err)
		}
		v.cache[def] = schema
	}
	value, err := jsonschema.UnmarshalJSON(bytes.NewReader(raw))
	if err != nil {
		v.t.Fatalf("decode %s value %q: %v", def, raw, err)
	}
	return schema.Validate(value)
}

func (v *validator) must(def string, raw []byte) {
	v.t.Helper()
	if def == "" {
		v.t.Fatalf("no schema definition for %s", raw)
	}
	if err := v.check(def, raw); err != nil {
		v.t.Fatalf("%s rejects %s: %v", def, raw, err)
	}
}

func outboundParamsDef(method string) string {
	switch method {
	case "session/update":
		return "SessionNotification"
	case "session/request_permission":
		return "RequestPermissionRequest"
	}
	return ""
}

func responseDef(method string) string {
	switch method {
	case "initialize":
		return "InitializeResponse"
	case "session/new":
		return "NewSessionResponse"
	case "session/load":
		return "LoadSessionResponse"
	case "session/list":
		return "ListSessionsResponse"
	case "session/prompt":
		return "PromptResponse"
	}
	return ""
}

var inboundParamsDefs = map[string]string{
	"initialize":                "InitializeRequest",
	"authenticate":              "AuthenticateRequest",
	"session/new":               "NewSessionRequest",
	"session/load":              "LoadSessionRequest",
	"session/list":              "ListSessionsRequest",
	"session/prompt":            "PromptRequest",
	"session/cancel":            "CancelNotification",
	"session/set_mode":          "SetSessionModeRequest",
	"session/set_config_option": "SetSessionConfigOptionRequest",
}

type editor struct {
	t       *testing.T
	v       *validator
	in      *io.PipeWriter
	lines   chan []byte
	methods map[string]string
	nextID  int
}

func newEditor(t *testing.T, v *validator, in *io.PipeWriter, out *io.PipeReader) (*editor, <-chan struct{}) {
	t.Helper()
	e := &editor{t: t, v: v, in: in, lines: make(chan []byte, 1024), methods: make(map[string]string)}
	pumped := make(chan struct{})
	go func() {
		defer close(pumped)
		defer close(e.lines)
		scanner := bufio.NewScanner(out)
		scanner.Buffer(make([]byte, 0, 64<<10), maxLine)
		for scanner.Scan() {
			e.lines <- bytes.Clone(scanner.Bytes())
		}
	}()
	return e, pumped
}

func (e *editor) write(m message) {
	e.t.Helper()
	m.JSONRPC = "2.0"
	line, err := json.Marshal(m)
	if err != nil {
		e.t.Fatal(err)
	}
	if _, err = e.in.Write(append(line, '\n')); err != nil {
		e.t.Fatalf("write to adapter: %v", err)
	}
}

func (e *editor) raw(line string) {
	e.t.Helper()
	if _, err := e.in.Write([]byte(line + "\n")); err != nil {
		e.t.Fatalf("write to adapter: %v", err)
	}
}

func (e *editor) params(method string, params any) jsontext.Value {
	e.t.Helper()
	raw, err := json.Marshal(params)
	if err != nil {
		e.t.Fatal(err)
	}
	if def := inboundParamsDefs[method]; def != "" {
		e.v.must(def, raw)
	}
	return raw
}

func (e *editor) request(method string, params any) jsontext.Value {
	e.t.Helper()
	e.nextID++
	id := jsontext.Value(strconv.Itoa(e.nextID))
	e.methods[string(id)] = method
	e.write(message{ID: id, Method: method, Params: e.params(method, params)})
	return id
}

func (e *editor) notify(method string, params any) {
	e.t.Helper()
	e.write(message{Method: method, Params: e.params(method, params)})
}

func (e *editor) respond(id jsontext.Value, result any) {
	e.t.Helper()
	raw, err := json.Marshal(result)
	if err != nil {
		e.t.Fatal(err)
	}
	e.v.must("RequestPermissionResponse", raw)
	e.write(message{ID: id, Result: raw})
}

func (e *editor) next() message {
	e.t.Helper()
	select {
	case line, ok := <-e.lines:
		if !ok {
			e.t.Fatal("the adapter closed its output")
		}
		return e.inspect(line)
	case <-time.After(waitLimit):
		e.t.Fatal("no adapter message in time")
	}
	return message{}
}

func (e *editor) inspect(line []byte) message {
	e.t.Helper()
	var m message
	if err := json.Unmarshal(line, &m); err != nil {
		e.t.Fatalf("adapter wrote invalid JSON %q: %v", line, err)
	}
	var probe map[string]any
	if err := json.Unmarshal(line, &probe); err != nil || probe["jsonrpc"] != "2.0" {
		e.t.Fatalf("adapter message lacks jsonrpc 2.0: %s", line)
	}
	switch {
	case m.Method != "" && len(m.ID) > 0:
		e.v.must("AgentRequest", line)
		e.v.must(outboundParamsDef(m.Method), m.Params)
	case m.Method != "":
		e.v.must("AgentNotification", line)
		e.v.must(outboundParamsDef(m.Method), m.Params)
	default:
		e.inspectResponse(m, line)
	}
	return m
}

func (e *editor) inspectResponse(m message, line []byte) {
	e.t.Helper()
	e.v.must("AgentResponse", line)
	if m.Error != nil {
		raw, err := json.Marshal(m.Error)
		if err != nil {
			e.t.Fatal(err)
		}
		e.v.must("Error", raw)
		return
	}
	e.v.must(responseDef(e.methods[string(m.ID)]), m.Result)
}

func (e *editor) response(id jsontext.Value) message {
	e.t.Helper()
	m := e.next()
	if m.Method != "" || string(m.ID) != string(id) {
		e.t.Fatalf("expected the response to request %s, got %+v", id, m)
	}
	return m
}

func (e *editor) result(id jsontext.Value, into any) {
	e.t.Helper()
	m := e.response(id)
	if m.Error != nil {
		e.t.Fatalf("request %s failed: %+v", id, m.Error)
	}
	if err := json.Unmarshal(m.Result, into); err != nil {
		e.t.Fatalf("decode result %s: %v", m.Result, err)
	}
}

func (e *editor) failure(id jsontext.Value) wireError {
	e.t.Helper()
	m := e.response(id)
	if m.Error == nil {
		e.t.Fatalf("request %s succeeded with %s", id, m.Result)
	}
	return *m.Error
}

type updateNote struct {
	SessionID string         `json:"sessionId"`
	Update    map[string]any `json:"update"`
}

func (e *editor) update() updateNote {
	e.t.Helper()
	m := e.next()
	if m.Method != "session/update" || len(m.ID) > 0 {
		e.t.Fatalf("expected a session/update notification, got %+v", m)
	}
	var u updateNote
	if err := json.Unmarshal(m.Params, &u); err != nil {
		e.t.Fatal(err)
	}
	return u
}

type call struct {
	method string
	params jsontext.Value
}

type daemon struct {
	t       *testing.T
	conn    *rpc.Conn
	mu      sync.Mutex
	calls   []call
	seen    chan call
	scripts map[string]func(jsontext.Value) (any, error)
	threads int
}

func newDaemon(ctx context.Context, t *testing.T, rwc io.ReadWriteCloser) *daemon {
	t.Helper()
	d := &daemon{t: t, seen: make(chan call, 1024), scripts: make(map[string]func(jsontext.Value) (any, error))}
	d.script("initialize", func(jsontext.Value) (any, error) {
		return rpc.InitializeResult{UserAgent: "dalgona/test", CodexHome: "/home/test", PlatformFamily: "unix", PlatformOS: "linux"}, nil
	})
	d.script("thread/start", func(jsontext.Value) (any, error) {
		d.mu.Lock()
		d.threads++
		id := "t" + strconv.Itoa(d.threads)
		d.mu.Unlock()
		return rpc.ThreadStartResponse{Thread: rpc.Thread{ID: id}}, nil
	})
	d.script("turn/start", func(jsontext.Value) (any, error) {
		return rpc.TurnStartResponse{Turn: rpc.Turn{ID: "u1", Status: "inProgress"}}, nil
	})
	d.script("turn/interrupt", func(jsontext.Value) (any, error) { return struct{}{}, nil })
	d.conn = rpc.NewConn(ctx, rwc, d.handle)
	return d
}

func (d *daemon) script(method string, f func(jsontext.Value) (any, error)) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.scripts[method] = f
}

func (d *daemon) handle(_ context.Context, method string, params jsontext.Value) (jsontext.Value, error) {
	c := call{method: method, params: bytes.Clone(params)}
	d.mu.Lock()
	d.calls = append(d.calls, c)
	f := d.scripts[method]
	d.mu.Unlock()
	d.seen <- c
	if f == nil {
		return nil, &rpc.RPCError{Code: -32601, Message: "Method not found: " + method}
	}
	result, err := f(params)
	if err != nil {
		return nil, err
	}
	raw, err := json.Marshal(result)
	if err != nil {
		d.t.Errorf("encode %s result: %v", method, err)
		return nil, err
	}
	return raw, nil
}

func (d *daemon) await(method string) call {
	d.t.Helper()
	timeout := time.After(waitLimit)
	for {
		select {
		case c := <-d.seen:
			if c.method == method {
				return c
			}
		case <-timeout:
			d.t.Fatalf("the daemon never saw %s", method)
		}
	}
}

func (d *daemon) count(method string) int {
	d.mu.Lock()
	defer d.mu.Unlock()
	n := 0
	for _, c := range d.calls {
		if c.method == method {
			n++
		}
	}
	return n
}

func (d *daemon) notify(method string, params any) {
	d.t.Helper()
	if err := d.conn.Notify(method, params); err != nil {
		d.t.Fatalf("notify %s: %v", method, err)
	}
}

func paramsOf[T any](t *testing.T, c call) T {
	t.Helper()
	var value T
	if err := json.Unmarshal(c.params, &value); err != nil {
		t.Fatalf("decode %s params %s: %v", c.method, c.params, err)
	}
	return value
}

type fixture struct {
	t        *testing.T
	ed       *editor
	d        *daemon
	done     chan error
	pumped   <-chan struct{}
	cancel   context.CancelFunc
	dials    atomic.Int32
	finished sync.Once
	serveErr error
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	serverEnd, clientEnd := net.Pipe()
	f := &fixture{t: t, done: make(chan error, 1), cancel: cancel}
	f.d = newDaemon(ctx, t, serverEnd)
	inR, inW := io.Pipe()
	outR, outW := io.Pipe()
	opts := Options{Version: testVersion, Dial: func(context.Context) (io.ReadWriteCloser, error) {
		f.dials.Add(1)
		return clientEnd, nil
	}}
	go func() {
		f.done <- Serve(ctx, inR, outW, opts)
		_ = outW.Close()
	}()
	f.ed, f.pumped = newEditor(t, newValidator(t), inW, outR)
	t.Cleanup(f.close)
	return f
}

func (f *fixture) finish() error {
	f.finished.Do(func() {
		select {
		case f.serveErr = <-f.done:
		case <-time.After(waitLimit):
			f.serveErr = errors.New("Serve did not return after stdin closed")
		}
	})
	return f.serveErr
}

func (f *fixture) close() {
	_ = f.ed.in.Close()
	if err := f.finish(); err != nil {
		f.t.Errorf("Serve returned %v", err)
	}
	f.cancel()
	<-f.pumped
	_ = f.d.conn.Close()
}

func (f *fixture) initialize() {
	f.t.Helper()
	id := f.ed.request("initialize", map[string]any{"protocolVersion": 1, "clientCapabilities": map[string]any{}})
	var got map[string]any
	f.ed.result(id, &got)
}

func (f *fixture) newSession(cwd string, servers ...any) string {
	f.t.Helper()
	if servers == nil {
		servers = []any{}
	}
	id := f.ed.request("session/new", map[string]any{"cwd": cwd, "mcpServers": servers})
	var got struct {
		SessionID string `json:"sessionId"`
	}
	f.ed.result(id, &got)
	if got.SessionID == "" {
		f.t.Fatal("session/new returned no session id")
	}
	return got.SessionID
}

func (f *fixture) session() string {
	f.t.Helper()
	f.initialize()
	return f.newSession("/work")
}

func (f *fixture) prompt(sessionID string, blocks ...map[string]any) jsontext.Value {
	f.t.Helper()
	return f.ed.request("session/prompt", map[string]any{"sessionId": sessionID, "prompt": blocks})
}

func promptText(s string) map[string]any {
	return map[string]any{"type": "text", "text": s}
}
