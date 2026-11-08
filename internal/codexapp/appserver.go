package codexapp

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/netip"
	"strings"
	"sync"
	"time"

	"github.com/coder/websocket"
)

const (
	appServerPrefix       = "dalgona app-server: "
	webSocketMessageLimit = 64 << 20
	daemonLineBuffer      = 64 << 10
)

type sessionField int

const (
	fieldModel sessionField = iota
	fieldApprovalPolicy
	fieldSandbox
	sessionFieldCount
)

type sessionDefaults [sessionFieldCount]string

var sessionFieldNames = sessionDefaults{"model", "approvalPolicy", "sandbox"}

var quotedSessionFieldNames = sessionDefaults{`"model"`, `"approvalPolicy"`, `"sandbox"`}

const quotedThreadStart = `"thread/start"`

type appServerArgs struct {
	defaults       sessionDefaults
	listen         string
	subcommandSeen bool
}

var codexApprovalPolicies = map[string]string{
	"never":      "never",
	"on-request": "on-request",
	"on-failure": "on-request",
	"untrusted":  "untrusted",
	"yolo":       "never",
	"auto":       "on-request",
	"ask":        "untrusted",
}

var codexSandboxModes = map[string]string{
	"danger-full-access": "danger-full-access",
	"workspace-write":    "workspace-write",
	"read-only":          "workspace-write",
	"off":                "danger-full-access",
}

type configSetting struct {
	field  sessionField
	values map[string]string
}

var configSettings = map[string]configSetting{
	"model":           {field: fieldModel},
	"approval_policy": {field: fieldApprovalPolicy, values: codexApprovalPolicies},
	"sandbox_mode":    {field: fieldSandbox, values: codexSandboxModes},
}

func AppServerMain(ctx context.Context, args []string, stdio io.ReadWriter, stderr io.Writer, dial func(context.Context) (net.Conn, error)) int {
	parsed, err := parseAppServerArgs(args, stderr)
	if err != nil {
		fmt.Fprintf(stderr, "%s%v\n", appServerPrefix, err)
		return 2
	}
	if parsed.listen == "" {
		err = spliceStdio(ctx, stdio, dial, parsed.defaults)
	} else {
		err = serveWebSocket(ctx, parsed.listen, stderr, dial, parsed.defaults)
	}
	if err != nil {
		fmt.Fprintf(stderr, "%s%v\n", appServerPrefix, err)
		return 1
	}
	return 0
}

func parseAppServerArgs(args []string, notes io.Writer) (appServerArgs, error) {
	var parsed appServerArgs
	for i := 0; i < len(args); i++ {
		next, err := parsed.consume(args, i, notes)
		if err != nil {
			return appServerArgs{}, err
		}
		i = next
	}
	return parsed, nil
}

func (a *appServerArgs) consume(args []string, i int, notes io.Writer) (int, error) {
	name, value, inline := strings.Cut(args[i], "=")
	switch {
	case name == "--config" || name == "-c":
		raw, next, err := flagValue(args, i, name, value, inline)
		if err != nil {
			return i, err
		}
		return next, a.addConfig(raw, notes)
	case name == "--listen":
		raw, next, err := flagValue(args, i, name, value, inline)
		if err != nil {
			return i, err
		}
		a.listen, err = parseListen(raw)
		return next, err
	case args[i] == "app-server" && !a.subcommandSeen:
		a.subcommandSeen = true
	default:
		noteIgnoredArgument(notes, args[i])
	}
	return i, nil
}

func flagValue(args []string, i int, name, value string, inline bool) (string, int, error) {
	if inline {
		return value, i, nil
	}
	if i+1 >= len(args) {
		return "", i, fmt.Errorf("%s requires a value", name)
	}
	return args[i+1], i + 1, nil
}

func noteIgnoredArgument(notes io.Writer, arg string) {
	if strings.HasPrefix(arg, "-") {
		fmt.Fprintf(notes, "%signoring flag %s\n", appServerPrefix, arg)
		return
	}
	fmt.Fprintf(notes, "%signoring argument %s\n", appServerPrefix, arg)
}

// source: .references/codex/codex-rs/utils/cli/src/config_override.rs
func (a *appServerArgs) addConfig(raw string, notes io.Writer) error {
	key, value, ok := strings.Cut(raw, "=")
	key = strings.TrimSpace(key)
	if !ok || key == "" {
		return fmt.Errorf("invalid --config %q; want key=value", raw)
	}
	value = strings.Trim(strings.TrimSpace(value), `"'`)
	setting, known := configSettings[key]
	if !known {
		fmt.Fprintf(notes, "%signoring config key %s\n", appServerPrefix, key)
		return nil
	}
	mapped := value
	if setting.values != nil {
		mapped = setting.values[value]
	}
	if mapped == "" {
		fmt.Fprintf(notes, "%signoring config %s=%q\n", appServerPrefix, key, value)
		return nil
	}
	a.defaults[setting.field] = mapped
	return nil
}

// source: .references/codex/codex-rs/app-server-transport/src/transport/websocket.rs
func parseListen(raw string) (string, error) {
	if raw == "stdio://" {
		return "", nil
	}
	hostPort, ok := strings.CutPrefix(raw, "ws://")
	if !ok {
		return "", fmt.Errorf("unsupported --listen %q; use stdio:// or ws://IP:PORT", raw)
	}
	addr, err := netip.ParseAddrPort(hostPort)
	if err != nil {
		return "", fmt.Errorf("invalid --listen %q: %w", raw, err)
	}
	if !addr.Addr().IsLoopback() {
		return "", fmt.Errorf("refusing to start non-loopback websocket listener %s without auth; listen on a loopback address and use SSH port-forwarding", addr)
	}
	return addr.String(), nil
}

func spliceStdio(ctx context.Context, stdio io.ReadWriter, dial func(context.Context) (net.Conn, error), defaults sessionDefaults) error {
	conn, err := dial(ctx)
	if err != nil {
		return fmt.Errorf("connect to daemon: %w", err)
	}
	defer conn.Close()
	stop := context.AfterFunc(ctx, func() { _ = conn.Close() })
	defer stop()
	go func() {
		_ = relayClientLines(conn, stdio, newParamInjector(defaults))
		closeDaemonWrite(conn)
	}()
	_, err = io.Copy(stdio, conn)
	if ctx.Err() != nil || err == nil || errors.Is(err, net.ErrClosed) || errors.Is(err, io.ErrClosedPipe) {
		return nil
	}
	return fmt.Errorf("relay daemon output: %w", err)
}

func relayClientLines(conn net.Conn, client io.Reader, injector *paramInjector) error {
	reader := bufio.NewReaderSize(client, daemonLineBuffer)
	var spill []byte
	for {
		line, err := readRawLine(reader, &spill)
		if len(line) > 0 {
			if _, writeErr := conn.Write(injector.line(line)); writeErr != nil {
				return writeErr
			}
		}
		if err != nil {
			return err
		}
	}
}

func closeDaemonWrite(conn net.Conn) bool {
	half, ok := conn.(interface{ CloseWrite() error })
	return ok && half.CloseWrite() == nil
}

type webSocketRelay struct {
	dial     func(context.Context) (net.Conn, error)
	defaults sessionDefaults
	stderr   io.Writer
	mu       sync.Mutex
	closed   bool
	active   sync.WaitGroup
}

func serveWebSocket(ctx context.Context, addr string, stderr io.Writer, dial func(context.Context) (net.Conn, error), defaults sessionDefaults) error {
	var config net.ListenConfig
	listener, err := config.Listen(ctx, "tcp", addr)
	if err != nil {
		return fmt.Errorf("listen on ws://%s: %w", addr, err)
	}
	fmt.Fprintf(stderr, "%slistening on ws://%s\n", appServerPrefix, listener.Addr())
	relay := &webSocketRelay{dial: dial, defaults: defaults, stderr: stderr}
	server := &http.Server{
		Handler:           relay,
		ReadHeaderTimeout: 10 * time.Second,
		BaseContext:       func(net.Listener) context.Context { return ctx },
	}
	stop := context.AfterFunc(ctx, func() { _ = server.Close() })
	defer stop()
	err = server.Serve(listener)
	relay.shutdown()
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return fmt.Errorf("serve ws://%s: %w", addr, err)
}

func (r *webSocketRelay) begin() bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return false
	}
	r.active.Add(1)
	return true
}

func (r *webSocketRelay) shutdown() {
	r.mu.Lock()
	r.closed = true
	r.mu.Unlock()
	r.active.Wait()
}

// source: .references/codex/codex-rs/app-server-transport/src/transport/websocket.rs
func (r *webSocketRelay) ServeHTTP(w http.ResponseWriter, req *http.Request) {
	if req.Header.Get("Origin") != "" {
		http.Error(w, "websocket requests with an Origin header are refused", http.StatusForbidden)
		return
	}
	if !r.begin() {
		http.Error(w, "app-server is shutting down", http.StatusServiceUnavailable)
		return
	}
	defer r.active.Done()
	conn, err := r.dial(req.Context())
	if err != nil {
		fmt.Fprintf(r.stderr, "%sconnect to daemon: %v\n", appServerPrefix, err)
		http.Error(w, "daemon unavailable", http.StatusServiceUnavailable)
		return
	}
	defer conn.Close()
	ws, err := websocket.Accept(w, req, nil)
	if err != nil {
		return
	}
	ws.SetReadLimit(webSocketMessageLimit)
	spliceWebSocket(req.Context(), ws, conn, newParamInjector(r.defaults))
}

func spliceWebSocket(ctx context.Context, ws *websocket.Conn, conn net.Conn, injector *paramInjector) {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	stop := context.AfterFunc(ctx, func() { _ = conn.Close() })
	defer stop()
	done := make(chan struct{})
	go func() {
		defer close(done)
		defer cancel()
		relayLinesToWebSocket(ctx, conn, ws)
	}()
	relayWebSocketToLines(ctx, ws, conn, injector)
	if !closeDaemonWrite(conn) {
		cancel()
	}
	<-done
	_ = ws.CloseNow()
}

func relayWebSocketToLines(ctx context.Context, ws *websocket.Conn, conn net.Conn, injector *paramInjector) {
	var message bytes.Buffer
	for {
		kind, reader, err := ws.Reader(ctx)
		if err != nil {
			return
		}
		message.Reset()
		if _, err := message.ReadFrom(reader); err != nil {
			return
		}
		if kind != websocket.MessageText {
			continue
		}
		if err := writeMessageLine(conn, &message, injector); err != nil {
			return
		}
	}
}

func writeMessageLine(conn net.Conn, message *bytes.Buffer, injector *paramInjector) error {
	if bytes.IndexByte(message.Bytes(), '\n') >= 0 {
		value := jsontext.Value(message.Bytes())
		if err := value.Compact(); err != nil {
			return fmt.Errorf("compact websocket message: %w", err)
		}
		message.Reset()
		message.Write(value)
	}
	if rewritten, ok := injector.rewrite(message.Bytes()); ok {
		_, err := conn.Write(rewritten)
		return err
	}
	message.WriteByte('\n')
	_, err := conn.Write(message.Bytes())
	return err
}

func relayLinesToWebSocket(ctx context.Context, conn net.Conn, ws *websocket.Conn) {
	reader := bufio.NewReaderSize(conn, daemonLineBuffer)
	var spill []byte
	for {
		line, err := readRawLine(reader, &spill)
		line = trimLineEnd(line)
		if len(line) > 0 {
			if writeErr := ws.Write(ctx, websocket.MessageText, line); writeErr != nil {
				return
			}
		}
		if errors.Is(err, io.EOF) {
			_ = ws.Close(websocket.StatusNormalClosure, "")
		}
		if err != nil {
			return
		}
	}
}

func readRawLine(reader *bufio.Reader, spill *[]byte) ([]byte, error) {
	chunk, err := reader.ReadSlice('\n')
	if !errors.Is(err, bufio.ErrBufferFull) {
		return chunk, err
	}
	*spill = append((*spill)[:0], chunk...)
	for errors.Is(err, bufio.ErrBufferFull) {
		chunk, err = reader.ReadSlice('\n')
		*spill = append(*spill, chunk...)
	}
	return *spill, err
}

func trimLineEnd(line []byte) []byte {
	line = bytes.TrimSuffix(line, []byte{'\n'})
	return bytes.TrimSuffix(line, []byte{'\r'})
}

type paramInjector struct {
	defaults sessionDefaults
	reader   bytes.Reader
	decoder  *jsontext.Decoder
	encoder  *jsontext.Encoder
	out      bytes.Buffer
	name     []byte
}

func newParamInjector(defaults sessionDefaults) *paramInjector {
	injector := &paramInjector{defaults: defaults}
	injector.decoder = jsontext.NewDecoder(&injector.reader)
	injector.encoder = jsontext.NewEncoder(&injector.out)
	return injector
}

func (p *paramInjector) line(raw []byte) []byte {
	if rewritten, ok := p.rewrite(trimLineEnd(raw)); ok {
		return rewritten
	}
	return raw
}

func (p *paramInjector) rewrite(message []byte) ([]byte, bool) {
	if p.defaults == (sessionDefaults{}) || !bytes.Contains(message, []byte(quotedThreadStart)) || !p.startsThread(message) {
		return nil, false
	}
	if err := p.inject(message); err != nil {
		return nil, false
	}
	return p.out.Bytes(), true
}

func (p *paramInjector) restart(message []byte) {
	p.reader.Reset(message)
	p.decoder.Reset(&p.reader)
}

func (p *paramInjector) startsThread(message []byte) bool {
	p.restart(message)
	if token, err := p.decoder.ReadToken(); err != nil || token.Kind() != jsontext.KindBeginObject {
		return false
	}
	for p.decoder.PeekKind() == jsontext.KindString {
		name, err := p.decoder.ReadValue()
		if err != nil {
			return false
		}
		if string(name) == `"method"` {
			method, err := p.decoder.ReadValue()
			return err == nil && string(method) == quotedThreadStart
		}
		if p.decoder.SkipValue() != nil {
			return false
		}
	}
	return false
}

func (p *paramInjector) inject(message []byte) error {
	p.restart(message)
	p.out.Reset()
	p.encoder.Reset(&p.out)
	if err := p.copyToken(); err != nil {
		return err
	}
	sawParams, err := p.copyRequestMembers()
	if err != nil {
		return err
	}
	if !sawParams {
		if err := errors.Join(p.encoder.WriteToken(jsontext.String("params")), p.writeDefaultsObject()); err != nil {
			return err
		}
	}
	if err := p.copyToken(); err != nil {
		return err
	}
	if _, err := p.decoder.ReadToken(); !errors.Is(err, io.EOF) {
		return fmt.Errorf("trailing data after message: %w", err)
	}
	return nil
}

func (p *paramInjector) copyRequestMembers() (bool, error) {
	sawParams := false
	for p.decoder.PeekKind() == jsontext.KindString {
		isParams, err := p.copyName(`"params"`)
		if err == nil && isParams {
			sawParams = true
			err = p.copyParams()
		} else if err == nil {
			err = p.copyValue()
		}
		if err != nil {
			return false, err
		}
	}
	return sawParams, nil
}

func (p *paramInjector) copyToken() error {
	token, err := p.decoder.ReadToken()
	if err != nil {
		return err
	}
	return p.encoder.WriteToken(token)
}

func (p *paramInjector) copyValue() error {
	value, err := p.decoder.ReadValue()
	if err != nil {
		return err
	}
	return p.encoder.WriteValue(value)
}

func (p *paramInjector) copyName(wanted string) (bool, error) {
	name, err := p.decoder.ReadValue()
	if err != nil {
		return false, err
	}
	matched := string(name) == wanted
	return matched, p.encoder.WriteValue(name)
}

func (p *paramInjector) copyParams() error {
	kind := p.decoder.PeekKind()
	if kind == jsontext.KindNull {
		if _, err := p.decoder.ReadToken(); err != nil {
			return err
		}
		return p.writeDefaultsObject()
	}
	if kind != jsontext.KindBeginObject {
		return p.copyValue()
	}
	if err := p.copyToken(); err != nil {
		return err
	}
	var present [sessionFieldCount]bool
	for p.decoder.PeekKind() == jsontext.KindString {
		if err := p.copyParamsMember(&present); err != nil {
			return err
		}
	}
	if err := p.writeMissingDefaults(present); err != nil {
		return err
	}
	return p.copyToken()
}

func (p *paramInjector) copyParamsMember(present *[sessionFieldCount]bool) error {
	name, err := p.decoder.ReadValue()
	if err != nil {
		return err
	}
	p.name = append(p.name[:0], name...)
	field, isSessionField := sessionFieldNamed(p.name)
	if isSessionField && p.defaults[field] != "" && p.decoder.PeekKind() == jsontext.KindNull {
		_, err := p.decoder.ReadToken()
		return err
	}
	if isSessionField {
		present[field] = true
	}
	if err := p.encoder.WriteValue(p.name); err != nil {
		return err
	}
	return p.copyValue()
}

func sessionFieldNamed(name []byte) (sessionField, bool) {
	for field, quoted := range quotedSessionFieldNames {
		if string(name) == quoted {
			return sessionField(field), true
		}
	}
	return 0, false
}

func (p *paramInjector) writeDefaultsObject() error {
	return errors.Join(
		p.encoder.WriteToken(jsontext.BeginObject),
		p.writeMissingDefaults([sessionFieldCount]bool{}),
		p.encoder.WriteToken(jsontext.EndObject),
	)
}

func (p *paramInjector) writeMissingDefaults(present [sessionFieldCount]bool) error {
	var err error
	for field, value := range p.defaults {
		if value == "" || present[field] {
			continue
		}
		err = errors.Join(err,
			p.encoder.WriteToken(jsontext.String(sessionFieldNames[field])),
			p.encoder.WriteToken(jsontext.String(value)))
	}
	return err
}
