package codexapp

import (
	"bufio"
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/netip"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/coder/websocket"

	"github.com/gosuda/dalgona/internal/auth"
)

const (
	appServerPrefix       = "dalgona app-server: "
	webSocketMessageLimit = 64 << 20
	daemonLineBuffer      = 64 << 10
	webSocketTokenBytes   = 32
	wsTokenFileName       = "app-server-token"
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
	ws             wsAuthArgs
}

type Deps struct {
	Home string
	Dial func(context.Context) (net.Conn, error)
}

type wsAuthArgs struct {
	mode        string
	tokenFile   string
	tokenSHA256 string
}

const (
	wsAuthCapabilityToken   = "capability-token"
	wsAuthSignedBearerToken = "signed-bearer-token"
)

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

func AppServerMain(ctx context.Context, args []string, stdio io.ReadWriter, stderr io.Writer, deps Deps) int {
	parsed, err := parseAppServerArgs(args, stderr)
	if err != nil {
		fmt.Fprintf(stderr, "%s%v\n", appServerPrefix, err)
		return 2
	}
	if parsed.listen == "" {
		err = spliceStdio(ctx, stdio, deps.Dial, parsed.defaults)
	} else {
		err = serveWebSocket(ctx, wsServerConfig{
			addr:     parsed.listen,
			home:     deps.Home,
			stderr:   stderr,
			dial:     deps.Dial,
			defaults: parsed.defaults,
			auth:     parsed.ws,
		})
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
	if err := parsed.validateWSAuth(); err != nil {
		return appServerArgs{}, err
	}
	return parsed, nil
}

func (a *appServerArgs) consume(args []string, i int, notes io.Writer) (int, error) {
	name, value, inline := strings.Cut(args[i], "=")
	switch {
	case isConfigFlag(name):
		raw, next, err := flagValue(args, i, name, value, inline)
		if err != nil {
			return i, err
		}
		return next, a.addConfig(raw, notes)
	case name == "--listen":
		return a.consumeListen(args, i, name, value, inline)
	case isWSAuthFlag(name):
		raw, next, err := flagValue(args, i, name, value, inline)
		if err != nil {
			return i, err
		}
		a.setWSAuth(name, raw)
		return next, nil
	case args[i] == "app-server" && !a.subcommandSeen:
		a.subcommandSeen = true
	default:
		noteIgnoredArgument(notes, args[i])
	}
	return i, nil
}

func isConfigFlag(name string) bool {
	return name == "--config" || name == "-c"
}

func isWSAuthFlag(name string) bool {
	switch name {
	case "--ws-auth", "--ws-token-file", "--ws-token-sha256":
		return true
	}
	return false
}

func (a *appServerArgs) consumeListen(args []string, i int, name, value string, inline bool) (int, error) {
	raw, next, err := flagValue(args, i, name, value, inline)
	if err != nil {
		return i, err
	}
	a.listen, err = parseListen(raw)
	return next, err
}

func (a *appServerArgs) setWSAuth(name, raw string) {
	switch name {
	case "--ws-auth":
		a.ws.mode = raw
	case "--ws-token-file":
		a.ws.tokenFile = raw
	default:
		a.ws.tokenSHA256 = raw
	}
}

func (a *appServerArgs) validateWSAuth() error {
	switch a.ws.mode {
	case "":
		if a.ws.tokenFile != "" || a.ws.tokenSHA256 != "" {
			return errors.New("websocket auth flags require --ws-auth capability-token")
		}
	case wsAuthCapabilityToken:
		return a.ws.validateCapabilityToken()
	case wsAuthSignedBearerToken:
		return errors.New("--ws-auth signed-bearer-token is not supported; use --ws-auth capability-token")
	default:
		return fmt.Errorf("unsupported --ws-auth %q; use capability-token", a.ws.mode)
	}
	return nil
}

func (a wsAuthArgs) validateCapabilityToken() error {
	if a.tokenFile != "" && a.tokenSHA256 != "" {
		return errors.New("--ws-token-file and --ws-token-sha256 are mutually exclusive")
	}
	if a.tokenFile == "" && a.tokenSHA256 == "" {
		return errors.New("--ws-token-file or --ws-token-sha256 is required when --ws-auth capability-token is set")
	}
	if a.tokenFile != "" {
		if !filepath.IsAbs(a.tokenFile) {
			return fmt.Errorf("--ws-token-file must be an absolute path: %q", a.tokenFile)
		}
		return nil
	}
	sum, err := hex.DecodeString(a.tokenSHA256)
	if err != nil || len(sum) != sha256.Size {
		return fmt.Errorf("--ws-token-sha256 must be 64-character hex of the token SHA-256: %q", a.tokenSHA256)
	}
	return nil
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

func (a *appServerArgs) addConfig(raw string, notes io.Writer) error {
	key, value, ok := strings.Cut(raw, "=")
	key = strings.TrimSpace(key)
	if !ok || key == "" {
		return fmt.Errorf("invalid --config %q; want key=value", raw)
	}
	value = strings.Trim(strings.TrimSpace(value), `"'`)
	setting, known := configSettings[key]
	if !known {
		_, _ = fmt.Fprintf(notes, "%signoring config key %s\n", appServerPrefix, key)
		return nil
	}
	mapped := value
	if setting.values != nil {
		mapped = setting.values[value]
	}
	if mapped == "" {
		_, _ = fmt.Fprintf(notes, "%signoring config %s=%q\n", appServerPrefix, key, value)
		return nil
	}
	a.defaults[setting.field] = mapped
	return nil
}

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
	dial        func(context.Context) (net.Conn, error)
	defaults    sessionDefaults
	stderr      io.Writer
	tokenDigest [sha256.Size]byte
	mu          sync.Mutex
	closed      bool
	active      sync.WaitGroup
}

type wsServerConfig struct {
	addr     string
	home     string
	stderr   io.Writer
	dial     func(context.Context) (net.Conn, error)
	defaults sessionDefaults
	auth     wsAuthArgs
}

func serveWebSocket(ctx context.Context, cfg wsServerConfig) error {
	var listenConfig net.ListenConfig
	listener, err := listenConfig.Listen(ctx, "tcp", cfg.addr)
	if err != nil {
		return fmt.Errorf("listen on ws://%s: %w", cfg.addr, err)
	}
	fmt.Fprintf(cfg.stderr, "%slistening on ws://%s\n", appServerPrefix, listener.Addr())
	digest, err := resolveWSTokenDigest(cfg.stderr, cfg.home, cfg.auth)
	if err != nil {
		_ = listener.Close()
		return err
	}
	relay := &webSocketRelay{dial: cfg.dial, defaults: cfg.defaults, stderr: cfg.stderr, tokenDigest: digest}
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
	return fmt.Errorf("serve ws://%s: %w", cfg.addr, err)
}

func resolveWSTokenDigest(stderr io.Writer, home string, auth wsAuthArgs) ([sha256.Size]byte, error) {
	if auth.mode == "" {
		return writeDefaultWSToken(stderr, home)
	}
	if auth.tokenFile != "" {
		raw, err := os.ReadFile(auth.tokenFile)
		if err != nil {
			return [sha256.Size]byte{}, fmt.Errorf("read --ws-token-file: %w", err)
		}
		token := strings.TrimSpace(string(raw))
		if token == "" {
			return [sha256.Size]byte{}, errors.New("--ws-token-file is empty")
		}
		return sha256.Sum256([]byte(token)), nil
	}
	sum, err := hex.DecodeString(auth.tokenSHA256)
	if err != nil || len(sum) != sha256.Size {
		return [sha256.Size]byte{}, fmt.Errorf("--ws-token-sha256 must be 64-character hex of the token SHA-256: %q", auth.tokenSHA256)
	}
	var digest [sha256.Size]byte
	copy(digest[:], sum)
	return digest, nil
}

func writeDefaultWSToken(stderr io.Writer, home string) ([sha256.Size]byte, error) {
	var none [sha256.Size]byte
	if home == "" {
		return none, errors.New("ws token file needs a daemon home")
	}
	raw := make([]byte, webSocketTokenBytes)
	if _, err := rand.Read(raw); err != nil {
		return none, fmt.Errorf("generate ws token: %w", err)
	}
	token := base64.RawURLEncoding.EncodeToString(raw)
	if homeErr := auth.ProtectHomeDir(home); homeErr != nil {
		return none, fmt.Errorf("create token dir: %w", homeErr)
	}
	path := filepath.Join(home, wsTokenFileName)
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, []byte(token+"\n"), 0o600); err != nil {
		return none, fmt.Errorf("write ws token file: %w", err)
	}
	if err := os.Rename(tmp, path); err != nil {
		return none, fmt.Errorf("publish ws token file: %w", err)
	}
	_, _ = fmt.Fprintf(stderr, "%stoken file %s\n", appServerPrefix, path)
	return sha256.Sum256([]byte(token)), nil
}

func authorizedBearerToken(header string, want [sha256.Size]byte) bool {
	token, ok := strings.CutPrefix(header, "Bearer ")
	if !ok || token == "" {
		return false
	}
	digest := sha256.Sum256([]byte(token))
	return subtle.ConstantTimeCompare(digest[:], want[:]) == 1
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

func (r *webSocketRelay) ServeHTTP(w http.ResponseWriter, req *http.Request) {
	if req.Header.Get("Origin") != "" {
		http.Error(w, "websocket requests with an Origin header are refused", http.StatusForbidden)
		return
	}
	if !authorizedBearerToken(req.Header.Get("Authorization"), r.tokenDigest) {
		http.Error(w, "websocket authorization required", http.StatusUnauthorized)
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
