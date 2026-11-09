package webui

import (
	"bufio"
	"bytes"
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"errors"
	"io"
	"math/big"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/fstest"
	"time"

	"github.com/coder/websocket"
)

type lineSink chan string

func (s lineSink) Write(p []byte) (int, error) {
	s <- string(p)
	return len(p), nil
}

type running struct {
	base     string
	token    string
	lines    lineSink
	stop     func()
	done     chan error
	stopOnce sync.Once
}

func builtSite() fstest.MapFS {
	return fstest.MapFS{"index.html": {Data: []byte("<!doctype html><title>dalgona</title>")}}
}

func refuseDial(context.Context) (net.Conn, error) {
	return nil, errors.New("no daemon in this test")
}

func start(t *testing.T, cfg Config, site fstest.MapFS, dial Dialer) *running {
	t.Helper()
	ctx, cancel := context.WithCancel(t.Context())
	r := &running{lines: make(lineSink, 8), stop: cancel, done: make(chan error, 1)}
	go func() { r.done <- serve(ctx, cfg, site, dial, r.lines) }()
	for {
		select {
		case line := <-r.lines:
			base, token, found := strings.Cut(strings.TrimSuffix(line, "\n"), "/#token=")
			if !found {
				continue
			}
			r.base, r.token = base, token
			t.Cleanup(func() { r.shutdown(t) })
			return r
		case err := <-r.done:
			t.Fatalf("serve returned before announcing: %v", err)
		case <-time.After(5 * time.Second):
			t.Fatal("serve did not announce a URL")
		}
	}
}

func (r *running) shutdown(t *testing.T) {
	t.Helper()
	r.stopOnce.Do(func() {
		r.stop()
		if err := <-r.done; err != nil {
			t.Errorf("serve after cancel: %v", err)
		}
	})
}

func (r *running) rpcURL(token string) string {
	return "ws" + strings.TrimPrefix(r.base, "http") + "/rpc?token=" + url.QueryEscape(token)
}

func TestNonLoopbackListenNeedsLongToken(t *testing.T) {
	cases := []struct{ listen, token string }{
		{"0.0.0.0:8080", ""},
		{"192.168.1.20:8080", strings.Repeat("k", 31)},
		{":8080", ""},
		{"[::]:8080", strings.Repeat("ü", 31)},
	}
	for _, c := range cases {
		err := serve(t.Context(), Config{Listen: c.listen, Token: c.token}, builtSite(), refuseDial, io.Discard)
		want := "web: listening on " + c.listen + " needs [web] token (32+ characters)"
		if err == nil || err.Error() != want {
			t.Errorf("listen %q token len %d: got %v, want %q", c.listen, len(c.token), err, want)
		}
	}
}

func TestLoopbackGeneratesFreshToken(t *testing.T) {
	first := start(t, Config{Listen: "127.0.0.1:0"}, builtSite(), refuseDial)
	second := start(t, Config{Listen: "127.0.0.1:0"}, builtSite(), refuseDial)
	if len(first.token) < minLANTokenChars || first.token == second.token {
		t.Fatalf("tokens %q and %q must be distinct and at least %d characters", first.token, second.token, minLANTokenChars)
	}
	if !strings.HasPrefix(first.base, "http://127.0.0.1:") {
		t.Fatalf("announced base %q", first.base)
	}
}

func TestLANWithoutTLSWarnsOnce(t *testing.T) {
	token := strings.Repeat("t", 32)
	lines := make(lineSink, 8)
	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() { done <- serve(ctx, Config{Listen: "0.0.0.0:0", Token: token}, builtSite(), refuseDial, lines) }()
	warning := <-lines
	announced := <-lines
	cancel()
	if err := <-done; err != nil {
		t.Fatalf("serve: %v", err)
	}
	addr := strings.TrimPrefix(strings.TrimSuffix(announced, "/#token="+token+"\n"), "http://")
	want := "web: the token travels unencrypted on " + addr + "; set cert_file and key_file to use TLS\n"
	if warning != want {
		t.Fatalf("warning %q, want %q", warning, want)
	}
	if len(lines) != 0 {
		t.Fatalf("unexpected extra output %q", <-lines)
	}
}

func TestRPCRejectsWrongToken(t *testing.T) {
	var dialed atomic.Int32
	dial := func(context.Context) (net.Conn, error) {
		dialed.Add(1)
		return nil, errors.New("must not dial")
	}
	r := start(t, Config{Listen: "127.0.0.1:0"}, builtSite(), dial)
	for _, token := range []string{"", r.token + "x", r.token[:len(r.token)-1], strings.ToUpper(r.token)} {
		conn, resp, err := websocket.Dial(t.Context(), r.rpcURL(token), nil)
		if err == nil {
			_ = conn.CloseNow()
			t.Fatalf("token %q was accepted", token)
		}
		if resp == nil || resp.StatusCode != http.StatusUnauthorized {
			t.Fatalf("token %q: response %v, want 401", token, resp)
		}
	}
	if dialed.Load() != 0 {
		t.Fatalf("daemon dialed %d times for rejected tokens", dialed.Load())
	}
}

func pipeDial(daemonSide chan<- net.Conn) Dialer {
	return func(context.Context) (net.Conn, error) {
		web, daemon := net.Pipe()
		daemonSide <- daemon
		return web, nil
	}
}

func openRPC(t *testing.T) (*running, *websocket.Conn, net.Conn) {
	t.Helper()
	daemons := make(chan net.Conn, 1)
	r := start(t, Config{Listen: "127.0.0.1:0"}, builtSite(), pipeDial(daemons))
	browser, _, err := websocket.Dial(t.Context(), r.rpcURL(r.token), nil)
	if err != nil {
		t.Fatalf("dial rpc: %v", err)
	}
	t.Cleanup(func() { _ = browser.CloseNow() })
	browser.SetReadLimit(-1)
	return r, browser, <-daemons
}

func TestShutdownClosesLiveRelays(t *testing.T) {
	r, _, daemon := openRPC(t)
	r.shutdown(t)
	if err := daemon.SetReadDeadline(time.Now()); !errors.Is(err, io.ErrClosedPipe) {
		t.Fatalf("daemon pipe still open after serve returned: %v", err)
	}
	if _, err := daemon.Read(make([]byte, 1)); !errors.Is(err, io.EOF) {
		t.Fatalf("daemon conn after serve returned: %v, want EOF", err)
	}
}

func writeLinesThenTail(daemon net.Conn, lines []string, tail string) {
	for _, line := range lines {
		if _, err := io.WriteString(daemon, line+"\n"); err != nil {
			return
		}
	}
	_, _ = io.WriteString(daemon, tail)
	_ = daemon.Close()
}

func TestRPCRelaysLinesBothWays(t *testing.T) {
	_, browser, daemon := openRPC(t)
	request := `{"jsonrpc":"2.0","id":1,"method":"thread/start","params":{"cwd":"/tmp/ü \t"}}`
	if err := browser.Write(t.Context(), websocket.MessageText, []byte(request)); err != nil {
		t.Fatalf("browser write: %v", err)
	}
	got, readErr := bufio.NewReader(daemon).ReadString('\n')
	if readErr != nil || got != request+"\n" {
		t.Fatalf("daemon read %q, %v; want %q", got, readErr, request+"\n")
	}
	long := `{"method":"item/agentMessage/delta","params":{"delta":"` + strings.Repeat("ab", 100<<10) + `"}}`
	replies := []string{`{"id":1,"result":{}}`, long, "", `{"method":"turn/completed"}`}
	go writeLinesThenTail(daemon, replies, `{"truncated":`)
	for i, want := range replies {
		kind, message, err := browser.Read(t.Context())
		if err != nil || kind != websocket.MessageText || !bytes.Equal(message, []byte(want)) {
			t.Fatalf("message %d: kind %v len %d err %v; want len %d", i, kind, len(message), err, len(want))
		}
	}
	_, tail, err := browser.Read(t.Context())
	if websocket.CloseStatus(err) != websocket.StatusGoingAway {
		t.Fatalf("after daemon close got message %q err %v, want going-away close", tail, err)
	}
}

func TestRPCClosesOnFramingViolations(t *testing.T) {
	cases := []struct {
		kind    websocket.MessageType
		message string
		status  websocket.StatusCode
	}{
		{websocket.MessageText, "{\"id\":1,\n\"method\":\"x\"}", websocket.StatusInvalidFramePayloadData},
		{websocket.MessageBinary, `{"id":1,"method":"x"}`, websocket.StatusUnsupportedData},
	}
	for _, c := range cases {
		_, browser, daemon := openRPC(t)
		if err := browser.Write(t.Context(), c.kind, []byte(c.message)); err != nil {
			t.Fatalf("browser write: %v", err)
		}
		forwarded, err := io.ReadAll(daemon)
		if err != nil || len(forwarded) != 0 {
			t.Fatalf("daemon received %q, %v from a rejected message", forwarded, err)
		}
		if _, _, err := browser.Read(t.Context()); websocket.CloseStatus(err) != c.status {
			t.Fatalf("close %v, want status %v", err, c.status)
		}
	}
}

func TestDaemonDialFailureIsBadGateway(t *testing.T) {
	r := start(t, Config{Listen: "127.0.0.1:0"}, builtSite(), refuseDial)
	_, resp, err := websocket.Dial(t.Context(), r.rpcURL(r.token), nil)
	if err == nil || resp == nil || resp.StatusCode != http.StatusBadGateway {
		t.Fatalf("dial with no daemon: %v %v, want 502", resp, err)
	}
	if line := <-r.lines; line != "web: dial daemon: no daemon in this test\n" {
		t.Fatalf("logged %q", line)
	}
}

func get(t *testing.T, client *http.Client, target string) (int, string) {
	t.Helper()
	req, err := http.NewRequestWithContext(t.Context(), http.MethodGet, target, http.NoBody)
	if err != nil {
		t.Fatalf("request: %v", err)
	}
	resp, err := client.Do(req)
	if err != nil {
		t.Fatalf("get %s: %v", target, err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("read body: %v", err)
	}
	return resp.StatusCode, string(body)
}

func TestUnbuiltDistServes503(t *testing.T) {
	unbuilt := start(t, Config{Listen: "127.0.0.1:0"}, fstest.MapFS{".gitkeep": {}}, refuseDial)
	for _, page := range []string{"/", "/index.html", "/_app/version.json", "/.gitkeep"} {
		status, body := get(t, http.DefaultClient, unbuilt.base+page)
		if status != http.StatusServiceUnavailable || body != notBuiltMessage+"\n" {
			t.Fatalf("unbuilt %s: %d %q", page, status, body)
		}
	}
	built := start(t, Config{Listen: "127.0.0.1:0"}, builtSite(), refuseDial)
	status, body := get(t, http.DefaultClient, built.base+"/")
	if status != http.StatusOK || !strings.Contains(body, "<title>dalgona</title>") {
		t.Fatalf("built /: %d %q", status, body)
	}
}

func writeSelfSigned(t *testing.T) (certFile, keyFile string, pool *x509.CertPool) {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatalf("key: %v", err)
	}
	template := &x509.Certificate{
		SerialNumber: big.NewInt(1),
		Subject:      pkix.Name{CommonName: "dalgona test"},
		IPAddresses:  []net.IP{net.IPv4(127, 0, 0, 1)},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(time.Hour),
	}
	der, err := x509.CreateCertificate(rand.Reader, template, template, &key.PublicKey, key)
	if err != nil {
		t.Fatalf("certificate: %v", err)
	}
	keyDER, err := x509.MarshalECPrivateKey(key)
	if err != nil {
		t.Fatalf("marshal key: %v", err)
	}
	dir := t.TempDir()
	certFile, keyFile = filepath.Join(dir, "cert.pem"), filepath.Join(dir, "key.pem")
	if writeErr := os.WriteFile(certFile, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der}), 0o600); writeErr != nil {
		t.Fatalf("write cert: %v", writeErr)
	}
	if writeErr := os.WriteFile(keyFile, pem.EncodeToMemory(&pem.Block{Type: "EC PRIVATE KEY", Bytes: keyDER}), 0o600); writeErr != nil {
		t.Fatalf("write key: %v", writeErr)
	}
	cert, err := x509.ParseCertificate(der)
	if err != nil {
		t.Fatalf("parse cert: %v", err)
	}
	pool = x509.NewCertPool()
	pool.AddCert(cert)
	return certFile, keyFile, pool
}

func TestTLSRequiresVersion13(t *testing.T) {
	certFile, keyFile, pool := writeSelfSigned(t)
	r := start(t, Config{Listen: "127.0.0.1:0", CertFile: certFile, KeyFile: keyFile}, builtSite(), refuseDial)
	if !strings.HasPrefix(r.base, "https://") {
		t.Fatalf("announced base %q, want https", r.base)
	}
	legacy := &tls.Config{RootCAs: pool, MinVersion: tls.VersionTLS12, MaxVersion: tls.VersionTLS12} //nolint:gosec // proves the server refuses TLS 1.2
	tls12 := &http.Client{Transport: &http.Transport{TLSClientConfig: legacy}}
	req, err := http.NewRequestWithContext(t.Context(), http.MethodGet, r.base+"/", http.NoBody)
	if err != nil {
		t.Fatalf("request: %v", err)
	}
	if resp, err := tls12.Do(req); err == nil {
		_ = resp.Body.Close()
		t.Fatal("TLS 1.2 handshake succeeded")
	}
	tls13 := &http.Client{Transport: &http.Transport{TLSClientConfig: &tls.Config{RootCAs: pool, MinVersion: tls.VersionTLS13}}}
	if status, _ := get(t, tls13, r.base+"/"); status != http.StatusOK {
		t.Fatalf("TLS 1.3 get status %d", status)
	}
}

func TestTLSConfigErrors(t *testing.T) {
	certFile, _, _ := writeSelfSigned(t)
	err := serve(t.Context(), Config{Listen: "127.0.0.1:0", CertFile: certFile}, builtSite(), refuseDial, io.Discard)
	if !errors.Is(err, errPartialTLS) {
		t.Fatalf("cert without key: %v", err)
	}
	err = serve(t.Context(), Config{Listen: "127.0.0.1:0", CertFile: certFile, KeyFile: certFile}, builtSite(), refuseDial, io.Discard)
	if err == nil || !strings.HasPrefix(err.Error(), "web: load cert_file and key_file: ") {
		t.Fatalf("cert as key: %v", err)
	}
}
