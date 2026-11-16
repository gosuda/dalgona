package acp

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"sync"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	protocolVersion = 1
	maxLine         = 16 << 20
	maxHandlers     = 64
	clientName      = "dalgona-acp"
)

type Options struct {
	Version string
	Dial    func(context.Context) (io.ReadWriteCloser, error)
}

type reply struct {
	result any
	after  func()
}

type handler func(*adapter, context.Context, jsontext.Value) (reply, error)

type adapter struct {
	opts      Options
	log       *slog.Logger
	peer      *peer
	slots     chan struct{}
	handlers  sync.WaitGroup
	connectMu sync.Mutex
	mu        sync.Mutex
	daemon    *daemonConn
	sessions  map[string]*session
}

type daemonConn struct {
	client *rpc.Conn
	lost   <-chan struct{}
}

type watchedConn struct {
	io.ReadWriteCloser
	lost chan struct{}
	once sync.Once
}

func (w *watchedConn) Read(p []byte) (int, error) {
	n, err := w.ReadWriteCloser.Read(p)
	if err != nil {
		w.once.Do(func() { close(w.lost) })
	}
	return n, err
}

func Serve(ctx context.Context, in io.Reader, out io.Writer, opts Options) error {
	if opts.Dial == nil {
		return errors.New("acp: serve needs a dialer")
	}
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	a := &adapter{opts: opts, log: slog.Default(), peer: newPeer(out), slots: make(chan struct{}, maxHandlers), sessions: make(map[string]*session)}
	stop := func() bool { return false }
	if closer, ok := in.(io.Closer); ok {
		stop = context.AfterFunc(ctx, func() { _ = closer.Close() })
	}
	err := a.read(ctx, in)
	stop()
	interrupted := ctx.Err() != nil
	a.peer.shutdown()
	cancel()
	a.handlers.Wait()
	a.closeDaemon()
	if interrupted {
		return nil
	}
	return err
}

func (a *adapter) read(ctx context.Context, in io.Reader) error {
	scanner := bufio.NewScanner(in)
	scanner.Buffer(make([]byte, 0, 64<<10), maxLine)
	for scanner.Scan() {
		line := bytes.TrimSpace(scanner.Bytes())
		if len(line) > 0 {
			a.dispatch(ctx, bytes.Clone(line))
		}
	}
	if err := scanner.Err(); err != nil {
		return fmt.Errorf("read editor input: %w", err)
	}
	return nil
}

func (a *adapter) dispatch(ctx context.Context, line []byte) {
	var m message
	if err := json.Unmarshal(line, &m); err != nil {
		a.reject(line)
		return
	}
	if m.Method == "" {
		if len(m.ID) > 0 && (len(m.Result) > 0 || m.Error != nil) {
			a.peer.resolve(m)
			return
		}
		a.rejectNonRequest(line, m.ID)
		return
	}
	select {
	case a.slots <- struct{}{}:
		a.handlers.Go(func() {
			defer func() { <-a.slots }()
			a.serve(ctx, m)
		})
	default:
		a.shed(m)
	}
}

func (a *adapter) shed(m message) {
	a.log.Warn("acp: too many open requests", "method", m.Method, "limit", maxHandlers)
	if len(m.ID) > 0 {
		a.logSend("shed request", a.peer.replyError(m.ID, rpcError(codeInternal, "adapter busy: too many open requests")))
	}
}

func (a *adapter) reject(line []byte) {
	var value any
	wire := rpcError(codeParse, "Parse error")
	if json.Unmarshal(line, &value) == nil {
		wire = rpcError(codeInvalidRequest, "Invalid request")
	}
	a.logSend("reject message", a.peer.replyError(jsontext.Value("null"), wire))
}

func (a *adapter) rejectNonRequest(line []byte, id jsontext.Value) {
	wire := rpcError(codeParse, "Parse error")
	var value any
	if json.Unmarshal(line, &value) == nil {
		wire = rpcError(codeInvalidRequest, "Invalid request")
	}
	a.logSend("reject message", a.peer.replyError(id, wire))
}

func (a *adapter) serve(ctx context.Context, m message) {
	r, err := route(m.Method)(a, ctx, m.Params)
	if len(m.ID) == 0 {
		if err != nil {
			a.log.Debug("acp: notification failed", "method", m.Method, "error", err)
		}
		return
	}
	if err != nil {
		a.logSend("reply error", a.peer.replyError(m.ID, err))
		return
	}
	sendErr := a.peer.replyResult(m.ID, r.result)
	a.logSend("reply result", sendErr)
	if sendErr == nil && r.after != nil {
		r.after()
	}
}

func (a *adapter) logSend(what string, err error) {
	if err != nil {
		a.log.Debug("acp: "+what, "error", err)
	}
}

func route(method string) handler {
	switch method {
	case "initialize":
		return (*adapter).initialize
	case "session/new":
		return (*adapter).newSession
	case "session/load":
		return (*adapter).loadSession
	case "session/list":
		return (*adapter).listSessions
	case "session/prompt":
		return (*adapter).prompt
	case "session/cancel":
		return (*adapter).cancel
	case "session/set_config_option":
		return (*adapter).setConfigOption
	}
	return (*adapter).methodNotFound
}

func (*adapter) methodNotFound(context.Context, jsontext.Value) (reply, error) {
	return reply{}, rpcError(codeMethodNotFound, "Method not found")
}

func decode[T any](params jsontext.Value) (T, error) {
	var value T
	if len(params) == 0 {
		params = jsontext.Value("{}")
	}
	if err := json.Unmarshal(params, &value); err != nil {
		return value, rpcError(codeInvalidParams, "invalid params: "+err.Error())
	}
	return value, nil
}

type initializeParams struct {
	ProtocolVersion *int `json:"protocolVersion"`
}

type (
	initializeResult struct {
		ProtocolVersion   int               `json:"protocolVersion"`
		AgentCapabilities agentCapabilities `json:"agentCapabilities"`
		AuthMethods       []struct{}        `json:"authMethods"`
		AgentInfo         implementation    `json:"agentInfo"`
	}
	agentCapabilities struct {
		LoadSession         bool                `json:"loadSession"`
		PromptCapabilities  promptCapabilities  `json:"promptCapabilities"`
		MCPCapabilities     mcpCapabilities     `json:"mcpCapabilities"`
		SessionCapabilities sessionCapabilities `json:"sessionCapabilities"`
	}
	promptCapabilities struct {
		Image bool `json:"image"`
	}
	mcpCapabilities struct {
		HTTP bool `json:"http"`
		SSE  bool `json:"sse"`
	}
	sessionCapabilities struct {
		List struct{} `json:"list"`
	}
	implementation struct {
		Name    string `json:"name"`
		Title   string `json:"title"`
		Version string `json:"version"`
	}
)

func newInitializeResult(version string) initializeResult {
	return initializeResult{
		ProtocolVersion: protocolVersion,
		AgentCapabilities: agentCapabilities{
			LoadSession:        true,
			PromptCapabilities: promptCapabilities{Image: true},
		},
		AuthMethods: []struct{}{},
		AgentInfo:   implementation{Name: "dalgona", Title: "dalgona", Version: version},
	}
}

func (a *adapter) initialize(ctx context.Context, params jsontext.Value) (reply, error) {
	p, err := decode[initializeParams](params)
	if err != nil {
		return reply{}, err
	}
	if p.ProtocolVersion == nil {
		return reply{}, rpcError(codeInvalidParams, "missing protocolVersion")
	}
	if connectErr := a.connect(ctx); connectErr != nil {
		return reply{}, rpcError(codeInternal, connectErr.Error())
	}
	return reply{result: newInitializeResult(a.opts.Version)}, nil
}

func (a *adapter) connect(ctx context.Context) error {
	a.connectMu.Lock()
	defer a.connectMu.Unlock()
	if _, err := a.conn(); err == nil {
		return nil
	}
	dialed, err := a.opts.Dial(ctx)
	if err != nil {
		return fmt.Errorf("dial daemon: %w", err)
	}
	conn := &watchedConn{ReadWriteCloser: dialed, lost: make(chan struct{})}
	client := rpc.Dial(conn, a)
	params := rpc.InitializeParams{
		ClientInfo:   rpc.ClientInfo{Name: clientName, Title: "dalgona ACP adapter", Version: a.opts.Version},
		Capabilities: &rpc.InitializeCapabilities{RequiredHumanApproval: true},
	}
	err = client.Initialize(ctx, params, &rpc.InitializeResult{})
	if err != nil {
		return errors.Join(fmt.Errorf("initialize daemon: %w", err), client.Close())
	}
	a.mu.Lock()
	a.daemon = &daemonConn{client: client, lost: conn.lost}
	a.mu.Unlock()
	return nil
}

func (a *adapter) conn() (*daemonConn, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.daemon == nil {
		return nil, rpcError(codeInvalidRequest, "initialize has not run")
	}
	return a.daemon, nil
}

func (a *adapter) closeDaemon() {
	a.mu.Lock()
	d := a.daemon
	a.daemon = nil
	a.mu.Unlock()
	if d == nil {
		return
	}
	if err := d.client.Close(); err != nil {
		a.log.Debug("acp: close daemon connection", "error", err)
	}
}
