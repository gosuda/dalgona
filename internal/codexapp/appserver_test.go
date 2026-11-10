package codexapp

import (
	"bufio"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
)

type stdioPair struct {
	io.Reader
	io.Writer
}

func replyDaemon(conn net.Conn) {
	defer conn.Close()
	if _, err := io.WriteString(conn, `{"method":"thread/started"}`+"\n"); err != nil {
		return
	}
	lines := bufio.NewScanner(conn)
	lines.Buffer(make([]byte, 0, 1<<20), 1<<20)
	for lines.Scan() {
		reply := append(append([]byte(`{"echo":`), lines.Bytes()...), "}\n"...)
		if _, err := conn.Write(reply); err != nil {
			return
		}
	}
}

func TestAppServerStdioRoundTrip(t *testing.T) {
	appIn, testIn := io.Pipe()
	testOut, appOut := io.Pipe()
	daemonDial := unixDaemon(t, replyDaemon)
	dials := 0
	dial := func(ctx context.Context) (net.Conn, error) {
		dials++
		return daemonDial(ctx)
	}
	var stderr bytes.Buffer
	done := make(chan int, 1)
	go func() {
		done <- AppServerMain(t.Context(), nil, stdioPair{appIn, appOut}, &stderr, Deps{Home: t.TempDir(), Dial: dial})
		_ = appOut.Close()
	}()

	large := `{"id":3,"method":"turn/start","params":{"text":"` + strings.Repeat("x", 200<<10) + `"}}`
	requests := []string{`{"id":1,"method":"initialize"}`, `{"method":"initialized"}`, large}
	go func() {
		for _, request := range requests {
			if _, err := io.WriteString(testIn, request+"\n"); err != nil {
				return
			}
		}
	}()

	output := bufio.NewReaderSize(testOut, 1<<20)
	want := []string{`{"method":"thread/started"}`}
	for _, request := range requests {
		want = append(want, `{"echo":`+request+`}`)
	}
	expectOutputLines(t, output, want)
	_ = testIn.Close()
	if code := <-done; code != 0 {
		t.Fatalf("AppServerMain = %d, stderr %q", code, stderr.String())
	}
	if dials != 1 || stderr.Len() != 0 {
		t.Fatalf("dials = %d, stderr = %q", dials, stderr.String())
	}
}

func expectOutputLines(t *testing.T, output *bufio.Reader, want []string) {
	t.Helper()
	for i, line := range want {
		got, err := output.ReadString('\n')
		if err != nil || got != line+"\n" {
			t.Fatalf("output line %d = %.80q, %v; want %.80q", i, got, err, line)
		}
	}
}

func replyAfterDelay(conn net.Conn, readRequest func(io.Reader) ([]byte, error)) {
	defer conn.Close()
	request, err := readRequest(conn)
	if err != nil {
		return
	}
	time.Sleep(100 * time.Millisecond)
	_, _ = conn.Write(append([]byte(`{"got":`), append(bytes.TrimSpace(request), "}\n"...)...))
}

func readOneLine(r io.Reader) ([]byte, error) {
	return bufio.NewReader(r).ReadBytes('\n')
}

func shortSocketPath(t *testing.T) string {
	t.Helper()
	dir, err := os.MkdirTemp("", "codexapp-sock-*")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return filepath.Join(dir, "d.sock")
}

func unixDaemon(t *testing.T, serve func(net.Conn)) func(context.Context) (net.Conn, error) {
	t.Helper()
	var listenConfig net.ListenConfig
	listener, err := listenConfig.Listen(t.Context(), "unix", shortSocketPath(t))
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	go func() {
		conn, err := listener.Accept()
		if err == nil {
			serve(conn)
		}
	}()
	return func(ctx context.Context) (net.Conn, error) {
		var dialer net.Dialer
		return dialer.DialContext(ctx, "unix", listener.Addr().String())
	}
}

func pipeDaemon(context.Context) (net.Conn, error) {
	daemonSide, appSide := net.Pipe()
	go replyAfterDelay(daemonSide, readOneLine)
	return appSide, nil
}

func TestAppServerStdioDeliversRepliesAfterClientEOF(t *testing.T) {
	daemons := map[string]func(*testing.T) func(context.Context) (net.Conn, error){
		"unix socket half-closed": func(t *testing.T) func(context.Context) (net.Conn, error) {
			return unixDaemon(t, func(conn net.Conn) { replyAfterDelay(conn, io.ReadAll) })
		},
		"pipe without half-close": func(*testing.T) func(context.Context) (net.Conn, error) { return pipeDaemon },
	}
	for name, daemon := range daemons {
		t.Run(name, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			stdio := stdioPair{strings.NewReader(`{"id":0,"method":"initialize"}` + "\n"), &stdout}
			args := []string{"--config", "approval_policy=untrusted", "app-server"}
			if code := AppServerMain(t.Context(), args, stdio, &stderr, Deps{Home: t.TempDir(), Dial: daemon(t)}); code != 0 {
				t.Fatalf("AppServerMain = %d, stderr %q", code, stderr.String())
			}
			if want := `{"got":{"id":0,"method":"initialize"}}` + "\n"; stdout.String() != want {
				t.Fatalf("stdout = %q, want %q", stdout.String(), want)
			}
		})
	}
}

func relayToDaemon(t *testing.T, args []string, input string) (received, notes string, code int) {
	t.Helper()
	got := make(chan []byte, 1)
	dial := unixDaemon(t, func(conn net.Conn) {
		data, _ := io.ReadAll(conn)
		_ = conn.Close()
		got <- data
	})
	var stderr bytes.Buffer
	code = AppServerMain(t.Context(), args, stdioPair{strings.NewReader(input), io.Discard}, &stderr, Deps{Home: t.TempDir(), Dial: dial})
	return string(<-got), stderr.String(), code
}

func TestAppServerConfigMappingAndIgnoredFlags(t *testing.T) {
	cases := []struct {
		name   string
		args   []string
		params string
		notes  string
	}{
		{
			name:   "codex values",
			args:   []string{"--config", "model=gpt-5.1", "-c", `approval_policy="never"`, "--config=sandbox_mode=read-only", "app-server", "--analytics-default-enabled", "--listen", "stdio://"},
			params: `{"model":"gpt-5.1","approvalPolicy":"never","sandbox":"workspace-write"}`,
			notes:  "dalgona app-server: ignoring flag --analytics-default-enabled\n",
		},
		{
			name:   "ignored keys and arguments",
			args:   []string{"-c", "model_reasoning_effort=high", "--config", "approval_policy=on-failure", "-c", "sandbox_mode=danger-full-access", "--stdio", "extra"},
			params: `{"approvalPolicy":"on-request","sandbox":"danger-full-access"}`,
			notes: "dalgona app-server: ignoring config key model_reasoning_effort\n" +
				"dalgona app-server: ignoring flag --stdio\n" +
				"dalgona app-server: ignoring argument extra\n",
		},
		{
			name:   "unknown values",
			args:   []string{"-c", "approval_policy=untrusted", "-c", "sandbox_mode=sideways", "app-server", "app-server"},
			params: `{"approvalPolicy":"untrusted"}`,
			notes: "dalgona app-server: ignoring config sandbox_mode=\"sideways\"\n" +
				"dalgona app-server: ignoring argument app-server\n",
		},
		{
			name:   "later value wins",
			args:   []string{"-c", "approval_policy=never", "-c", "approval_policy=on-request", "-c", "model="},
			params: `{"approvalPolicy":"on-request"}`,
			notes:  "dalgona app-server: ignoring config model=\"\"\n",
		},
		{
			name:   "dalgona values",
			args:   []string{"-c", "approval_policy=ask", "-c", "sandbox_mode=off"},
			params: `{"approvalPolicy":"untrusted","sandbox":"danger-full-access"}`,
		},
		{
			name:   "no settings",
			args:   []string{"--listen=stdio://"},
			params: `{}`,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			received, notes, code := relayToDaemon(t, tc.args, `{"id":1,"method":"thread/start","params":{}}`+"\n")
			if code != 0 {
				t.Fatalf("AppServerMain = %d, stderr %q", code, notes)
			}
			if want := `{"id":1,"method":"thread/start","params":` + tc.params + "}\n"; received != want {
				t.Fatalf("daemon received %q, want %q", received, want)
			}
			if notes != tc.notes {
				t.Fatalf("stderr = %q, want %q", notes, tc.notes)
			}
		})
	}
}

func TestAppServerInjectsSessionDefaults(t *testing.T) {
	args := []string{"-c", "model=m", "-c", "approval_policy=never", "-c", "sandbox_mode=danger-full-access"}
	injected := `"model":"m","approvalPolicy":"never","sandbox":"danger-full-access"`
	unchanged := func(line string) struct{ sent, received string } { return struct{ sent, received string }{line, line} }
	exchanges := []struct{ sent, received string }{
		{`{"id":1,"method":"thread/start","params":{"cwd":"/w"}}` + "\n", `{"id":1,"method":"thread/start","params":{"cwd":"/w",` + injected + `}}` + "\n"},
		{`{"id":2,"method":"thread/start","params":{"cwd":"/w","model":null,"sandbox":"workspace-write"}}` + "\r\n", `{"id":2,"method":"thread/start","params":{"cwd":"/w","sandbox":"workspace-write","model":"m","approvalPolicy":"never"}}` + "\n"},
		{`{"id":3,"method":"thread/start"}` + "\n", `{"id":3,"method":"thread/start","params":{` + injected + `}}` + "\n"},
		{`{"params":null,"method":"thread/start","id":4}` + "\n", `{"params":{` + injected + `},"method":"thread/start","id":4}` + "\n"},
		unchanged(`{"id":5,"method":"thread/start","params":{"model":"other","approvalPolicy":{"granular":{}},"sandbox":"workspace-write"}}` + "\n"),
		unchanged(`{"id":6,"method":"turn/start","params":{"threadId":"t","input":[]}}` + "\n"),
		unchanged(`{"id":7,"method":"thread/resume","params":{"threadId":"t"}}` + "\n"),
		unchanged(`{"id":8,"method":"thread/compact/start","params":{"threadId":"t"}}` + "\n"),
		unchanged(`{"id":9,"method":"turn/steer","params":{"note":"thread/start"}}` + "\n"),
		unchanged(`{ "id" : 10, "method" : "model/list" }` + "\r\n"),
		unchanged(`{"id":11,"method":"thread/start","params":{}} junk` + "\n"),
		unchanged(`{"id":12,"method":"thread/start","params":"odd"}` + "\n"),
		unchanged("not json \"thread/start\"\n"),
		unchanged(`{"id":13,"method":"model/list"}`),
	}
	var sent, want strings.Builder
	for _, exchange := range exchanges {
		sent.WriteString(exchange.sent)
		want.WriteString(exchange.received)
	}
	received, notes, code := relayToDaemon(t, args, sent.String())
	if code != 0 || notes != "" {
		t.Fatalf("AppServerMain = %d, stderr %q", code, notes)
	}
	if received != want.String() {
		t.Fatalf("daemon received\n%s\nwant\n%s", received, want.String())
	}
}

func TestAppServerRejectsInvalidArguments(t *testing.T) {
	cases := []struct {
		args    []string
		code    int
		message string
		dial    func(context.Context) (net.Conn, error)
	}{
		{
			args:    []string{"--listen", "ws://0.0.0.0:4500"},
			code:    2,
			message: "dalgona app-server: refusing to start non-loopback websocket listener 0.0.0.0:4500 without auth; listen on a loopback address and use SSH port-forwarding\n",
		},
		{
			args:    []string{"--listen", "unix://"},
			code:    2,
			message: "dalgona app-server: unsupported --listen \"unix://\"; use stdio:// or ws://IP:PORT\n",
		},
		{
			args:    []string{"--listen", "ws://localhost:4500"},
			code:    2,
			message: "dalgona app-server: invalid --listen \"ws://localhost:4500\": ParseAddr(\"localhost\"): unable to parse IP\n",
		},
		{args: []string{"--config"}, code: 2, message: "dalgona app-server: --config requires a value\n"},
		{args: []string{"--ws-auth"}, code: 2, message: "dalgona app-server: --ws-auth requires a value\n"},
		{args: []string{"--ws-auth", "bogus"}, code: 2, message: "dalgona app-server: unsupported --ws-auth \"bogus\"; use capability-token\n"},
		{args: []string{"--ws-auth", "signed-bearer-token"}, code: 2, message: "dalgona app-server: --ws-auth signed-bearer-token is not supported; use --ws-auth capability-token\n"},
		{args: []string{"--ws-auth", "capability-token"}, code: 2, message: "dalgona app-server: --ws-token-file or --ws-token-sha256 is required when --ws-auth capability-token is set\n"},
		{args: []string{"--ws-auth", "capability-token", "--ws-token-file", "a", "--ws-token-sha256", "b"}, code: 2, message: "dalgona app-server: --ws-token-file and --ws-token-sha256 are mutually exclusive\n"},
		{args: []string{"--ws-auth", "capability-token", "--ws-token-file", "relative/path"}, code: 2, message: "dalgona app-server: --ws-token-file must be an absolute path: \"relative/path\"\n"},
		{args: []string{"--ws-auth", "capability-token", "--ws-token-sha256", "not-hex"}, code: 2, message: "dalgona app-server: --ws-token-sha256 must be 64-character hex of the token SHA-256: \"not-hex\"\n"},
		{args: []string{"--ws-token-file", "/tmp/token"}, code: 2, message: "dalgona app-server: websocket auth flags require --ws-auth capability-token\n"},
		{
			args:    []string{"--listen", "stdio://"},
			code:    1,
			message: "dalgona app-server: connect to daemon: daemon down\n",
			dial:    func(context.Context) (net.Conn, error) { return nil, errors.New("daemon down") },
		},
	}
	for _, tc := range cases {
		dial := tc.dial
		if dial == nil {
			dial = func(context.Context) (net.Conn, error) {
				t.Errorf("%v: dial called", tc.args)
				return nil, errors.New("unexpected dial")
			}
		}
		var stderr bytes.Buffer
		code := AppServerMain(t.Context(), tc.args, stdioPair{strings.NewReader(""), io.Discard}, &stderr, Deps{Home: t.TempDir(), Dial: dial})
		if code != tc.code || stderr.String() != tc.message {
			t.Errorf("%v: code %d stderr %q, want %d %q", tc.args, code, stderr.String(), tc.code, tc.message)
		}
	}
}

type webSocketHarness struct {
	t         *testing.T
	url       string
	token     string
	tokenPath string
	daemons   chan net.Conn
}

func launchAppServer(t *testing.T, home string, args ...string) (webSocketHarness, *bufio.Reader, func() int) {
	t.Helper()
	ctx, cancel := context.WithCancel(t.Context())
	stderrRead, stderrWrite := io.Pipe()
	h := webSocketHarness{t: t, daemons: make(chan net.Conn, 8)}
	dial := func(context.Context) (net.Conn, error) {
		daemonSide, appSide := net.Pipe()
		h.daemons <- daemonSide
		return appSide, nil
	}
	done := make(chan int, 1)
	go func() {
		done <- AppServerMain(ctx, append([]string{"--listen", "ws://127.0.0.1:0"}, args...), stdioPair{strings.NewReader(""), io.Discard}, stderrWrite, Deps{Home: home, Dial: dial})
		_ = stderrWrite.Close()
	}()
	notes := bufio.NewReader(stderrRead)
	banner, err := notes.ReadString('\n')
	if err != nil {
		cancel()
		t.Fatalf("read banner: %v", err)
	}
	addr, ok := strings.CutPrefix(strings.TrimSpace(banner), "dalgona app-server: listening on ws://")
	if !ok {
		cancel()
		t.Fatalf("banner = %q", banner)
	}
	h.url = "ws://" + addr + "/"
	stop := func() int {
		cancel()
		return <-done
	}
	return h, notes, stop
}

func drainNotes(notes *bufio.Reader) {
	go func() { _, _ = io.Copy(io.Discard, notes) }()
}

func startWebSocketAppServer(t *testing.T, args ...string) (webSocketHarness, func() int) {
	t.Helper()
	h, notes, stop := launchAppServer(t, t.TempDir(), args...)
	second, err := notes.ReadString('\n')
	if err != nil {
		stop()
		t.Fatalf("read token line: %v", err)
	}
	drainNotes(notes)
	path, ok := strings.CutPrefix(strings.TrimSpace(second), "dalgona app-server: token file ")
	if !ok || path == "" {
		stop()
		t.Fatalf("token line = %q", second)
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		stop()
		t.Fatalf("read token file: %v", err)
	}
	h.tokenPath = path
	h.token = strings.TrimSpace(string(raw))
	if h.token == "" {
		stop()
		t.Fatal("token file is empty")
	}
	return h, stop
}

func startWebSocketAppServerWithToken(t *testing.T, token string, args ...string) (webSocketHarness, func() int) {
	t.Helper()
	h, notes, stop := launchAppServer(t, t.TempDir(), args...)
	drainNotes(notes)
	h.token = token
	return h, stop
}

func sha256Hex(token string) string {
	sum := sha256.Sum256([]byte(token))
	return hex.EncodeToString(sum[:])
}

func (h webSocketHarness) connect() (*websocket.Conn, *bufio.Reader, net.Conn) {
	h.t.Helper()
	options := &websocket.DialOptions{HTTPHeader: http.Header{"Authorization": {"Bearer " + h.token}}}
	ws, response, err := websocket.Dial(h.t.Context(), h.url, options)
	if err != nil {
		h.t.Fatalf("dial websocket: %v", err)
	}
	if response.Body != nil {
		_ = response.Body.Close()
	}
	ws.SetReadLimit(1 << 20)
	daemon := <-h.daemons
	return ws, bufio.NewReader(daemon), daemon
}

func (h webSocketHarness) send(ws *websocket.Conn, kind websocket.MessageType, message string) {
	h.t.Helper()
	if err := ws.Write(h.t.Context(), kind, []byte(message)); err != nil {
		h.t.Fatalf("write websocket: %v", err)
	}
}

func (h webSocketHarness) expectLine(daemon *bufio.Reader, want string) {
	h.t.Helper()
	got, err := daemon.ReadString('\n')
	if err != nil || got != want+"\n" {
		h.t.Fatalf("daemon line = %q, %v; want %q", got, err, want)
	}
}

func (h webSocketHarness) expectMessage(ws *websocket.Conn, want string) {
	h.t.Helper()
	kind, got, err := ws.Read(h.t.Context())
	if err != nil || kind != websocket.MessageText || string(got) != want {
		h.t.Fatalf("websocket message = %v %.80q %v; want text %.80q", kind, got, err, want)
	}
}

func TestAppServerWebSocketSplicesEachConnection(t *testing.T) {
	h, stop := startWebSocketAppServer(t, "-c", "model=m")

	first, firstDaemon, firstConn := h.connect()
	second, secondDaemon, secondConn := h.connect()

	h.send(first, websocket.MessageText, `{"id":1,"method":"initialize"}`)
	h.expectLine(firstDaemon, `{"id":1,"method":"initialize"}`)
	h.send(second, websocket.MessageText, `{"id":7,"method":"thread/list"}`)
	h.expectLine(secondDaemon, `{"id":7,"method":"thread/list"}`)
	h.send(first, websocket.MessageText, "{\n  \"id\": 2,\n  \"method\": \"model/list\"\n}")
	h.expectLine(firstDaemon, `{"id":2,"method":"model/list"}`)
	h.send(first, websocket.MessageBinary, `{"id":99}`)
	h.send(first, websocket.MessageText, `{"method":"initialized"}`)
	h.expectLine(firstDaemon, `{"method":"initialized"}`)
	largeRequest := `{"id":3,"method":"turn/start","params":{"text":"` + strings.Repeat("z", 200<<10) + `"}}`
	h.send(first, websocket.MessageText, largeRequest)
	h.expectLine(firstDaemon, largeRequest)
	h.send(first, websocket.MessageText, "{\n  \"id\": 4,\n  \"method\": \"thread/start\"\n}")
	h.expectLine(firstDaemon, `{"id":4,"method":"thread/start","params":{"model":"m"}}`)

	large := `{"method":"item/agentMessage/delta","params":{"delta":"` + strings.Repeat("y", 200<<10) + `"}}`
	go func() {
		_, _ = io.WriteString(firstConn, `{"id":1,"result":{}}`+"\n"+`{"method":"turn/started"}`+"\r\n"+large+"\n")
		_, _ = io.WriteString(secondConn, `{"id":7,"result":{"data":[]}}`+"\n")
	}()
	h.expectMessage(first, `{"id":1,"result":{}}`)
	h.expectMessage(first, `{"method":"turn/started"}`)
	h.expectMessage(first, large)
	h.expectMessage(second, `{"id":7,"result":{"data":[]}}`)

	_ = firstConn.Close()
	if _, _, err := first.Read(h.t.Context()); websocket.CloseStatus(err) != websocket.StatusNormalClosure {
		t.Fatalf("read after daemon close: %v", err)
	}
	if code := stop(); code != 0 {
		t.Fatalf("AppServerMain = %d", code)
	}
	if _, _, err := second.Read(t.Context()); err == nil {
		t.Fatal("second connection stayed open after shutdown")
	}
}

func TestAppServerWebSocketRefusesOriginHeader(t *testing.T) {
	h, stop := startWebSocketAppServer(t)
	defer stop()
	sameHost := "http://" + strings.TrimSuffix(strings.TrimPrefix(h.url, "ws://"), "/")
	options := &websocket.DialOptions{HTTPHeader: http.Header{"Origin": {sameHost}}}
	ws, response, err := websocket.Dial(h.t.Context(), h.url, options)
	if err == nil {
		_ = ws.CloseNow()
		t.Fatal("dial with Origin header succeeded")
	}
	if response == nil || response.StatusCode != http.StatusForbidden {
		t.Fatalf("response = %v, err = %v", response, err)
	}
	_ = response.Body.Close()
	select {
	case <-h.daemons:
		t.Fatal("refused request dialed the daemon")
	default:
	}
}

func TestAppServerWebSocketRequiresBearerToken(t *testing.T) {
	h, stop := startWebSocketAppServer(t)
	defer stop()
	for _, tc := range []struct {
		name   string
		header http.Header
	}{
		{name: "missing", header: nil},
		{name: "wrong", header: http.Header{"Authorization": {"Bearer wrong-token"}}},
		{name: "scheme", header: http.Header{"Authorization": {"Token " + h.token}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			options := &websocket.DialOptions{}
			if tc.header != nil {
				options.HTTPHeader = tc.header
			}
			ws, response, err := websocket.Dial(h.t.Context(), h.url, options)
			if err == nil {
				_ = ws.CloseNow()
				t.Fatal("unauthenticated dial succeeded")
			}
			if response == nil || response.StatusCode != http.StatusUnauthorized {
				t.Fatalf("response = %v, err = %v", response, err)
			}
			_ = response.Body.Close()
			select {
			case <-h.daemons:
				t.Fatal("rejected request dialed the daemon")
			default:
			}
		})
	}
}

func TestAppServerWebSocketWritesTokenFile(t *testing.T) {
	home := t.TempDir()
	h, notes, stop := launchAppServer(t, home)
	defer func() {
		if code := stop(); code != 0 {
			t.Fatalf("AppServerMain = %d", code)
		}
	}()
	second, err := notes.ReadString('\n')
	if err != nil {
		t.Fatalf("read token line: %v", err)
	}
	drainNotes(notes)
	want := filepath.Join(home, "app-server-token")
	path, ok := strings.CutPrefix(strings.TrimSpace(second), "dalgona app-server: token file ")
	if !ok || path != want {
		t.Fatalf("token line = %q, want token file %q", second, want)
	}
	token := readTokenFileToken(t, path)
	if strings.Contains(second, token) {
		t.Fatal("stderr leaks the raw token")
	}
	checkTokenFilePermissions(t, path)
	h.token = token
	checkWebSocketRoundTrip(h)
}

func readTokenFileToken(t *testing.T, path string) string {
	t.Helper()
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read token file: %v", err)
	}
	token := strings.TrimSpace(string(raw))
	if token == "" {
		t.Fatal("token file is empty")
	}
	return token
}

func checkTokenFilePermissions(t *testing.T, path string) {
	t.Helper()
	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat token file: %v", err)
	}
	if info.Mode().Perm() != 0o600 {
		t.Fatalf("token file mode = %o, want 600", info.Mode().Perm())
	}
}

func checkWebSocketRoundTrip(h webSocketHarness) {
	ws, daemon, conn := h.connect()
	defer conn.Close()
	h.send(ws, websocket.MessageText, `{"id":1,"method":"initialize"}`)
	h.expectLine(daemon, `{"id":1,"method":"initialize"}`)
	_ = ws.CloseNow()
}

func TestAppServerWebSocketCapabilityTokenFile(t *testing.T) {
	token := "file-secret-token"
	path := filepath.Join(t.TempDir(), "ws-token")
	if err := os.WriteFile(path, []byte(token+"\n"), 0o600); err != nil {
		t.Fatalf("write token file: %v", err)
	}
	h, stop := startWebSocketAppServerWithToken(t, token, "--ws-auth", "capability-token", "--ws-token-file", path)
	defer stop()
	ws, daemon, conn := h.connect()
	defer conn.Close()
	h.send(ws, websocket.MessageText, `{"id":1,"method":"initialize"}`)
	h.expectLine(daemon, `{"id":1,"method":"initialize"}`)
	_ = ws.CloseNow()

	bad, response, err := websocket.Dial(h.t.Context(), h.url, &websocket.DialOptions{HTTPHeader: http.Header{"Authorization": {"Bearer other"}}})
	if err == nil {
		_ = bad.CloseNow()
		t.Fatal("wrong token dial succeeded")
	}
	if response == nil || response.StatusCode != http.StatusUnauthorized {
		t.Fatalf("response = %v, err = %v", response, err)
	}
	_ = response.Body.Close()
	select {
	case <-h.daemons:
		t.Fatal("rejected request dialed the daemon")
	default:
	}
}

func TestAppServerWebSocketCapabilityTokenSHA256(t *testing.T) {
	token := "sha-secret-token"
	sum := sha256Hex(token)
	h, stop := startWebSocketAppServerWithToken(t, token, "--ws-auth", "capability-token", "--ws-token-sha256", sum)
	defer stop()
	ws, daemon, conn := h.connect()
	defer conn.Close()
	h.send(ws, websocket.MessageText, `{"id":1,"method":"initialize"}`)
	h.expectLine(daemon, `{"id":1,"method":"initialize"}`)
	_ = ws.CloseNow()
}
