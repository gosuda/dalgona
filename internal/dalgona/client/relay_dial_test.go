package client

import (
	"bufio"
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"io"
	"math/big"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/webui"
)

type relayFiles struct {
	dir, cert, key, token string
	bearer                string
	pair                  tls.Certificate
}

func writeRelayFiles(t *testing.T) relayFiles {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	template := x509.Certificate{
		SerialNumber:          big.NewInt(1),
		Subject:               pkix.Name{CommonName: "dalgona relay test"},
		IPAddresses:           []net.IP{net.IPv4(127, 0, 0, 1)},
		NotBefore:             time.Now().Add(-time.Hour),
		NotAfter:              time.Now().Add(time.Hour),
		KeyUsage:              x509.KeyUsageCertSign | x509.KeyUsageDigitalSignature,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		BasicConstraintsValid: true,
		IsCA:                  true,
	}
	der, err := x509.CreateCertificate(rand.Reader, &template, &template, &key.PublicKey, key)
	if err != nil {
		t.Fatal(err)
	}
	keyDER, err := x509.MarshalECPrivateKey(key)
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	files := relayFiles{dir: dir, cert: filepath.Join(dir, "cert.pem"), key: filepath.Join(dir, "key.pem"), token: filepath.Join(dir, "work.token"), bearer: rand.Text()}
	certPEM := pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der})
	keyPEM := pem.EncodeToMemory(&pem.Block{Type: "EC PRIVATE KEY", Bytes: keyDER})
	for path, data := range map[string][]byte{files.cert: certPEM, files.key: keyPEM, files.token: []byte(files.bearer + "\n")} {
		if writeErr := os.WriteFile(path, data, 0o600); writeErr != nil {
			t.Fatal(writeErr)
		}
	}
	if files.pair, err = tls.X509KeyPair(certPEM, keyPEM); err != nil {
		t.Fatal(err)
	}
	return files
}

func (f relayFiles) remotes(url, caFile string) *Remotes {
	cfg := config.Config{Home: f.dir, TLSCAFile: caFile, Remotes: map[string]config.RemoteSpec{
		"work": {Kind: config.RemoteDaemon, URL: url, TokenFile: f.token},
	}}
	return NewRemotes(cfg, nil)
}

type relayRequest struct {
	uri           string
	authorization string
}

func startRelayServer(t *testing.T, files relayFiles, maxVersion uint16, handler http.HandlerFunc) (*httptest.Server, <-chan relayRequest) {
	t.Helper()
	requests := make(chan relayRequest, 4)
	srv := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests <- relayRequest{uri: r.RequestURI, authorization: r.Header.Get("Authorization")}
		handler(w, r)
	}))
	srv.TLS = &tls.Config{Certificates: []tls.Certificate{files.pair}, MaxVersion: maxVersion}
	srv.StartTLS()
	t.Cleanup(srv.Close)
	return srv, requests
}

func refuseEveryToken(w http.ResponseWriter, _ *http.Request) {
	http.Error(w, http.StatusText(http.StatusUnauthorized), http.StatusUnauthorized)
}

func echoThreeMessages(received chan<- string) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		ws, err := websocket.Accept(w, r, nil)
		if err != nil {
			return
		}
		defer func() { _ = ws.CloseNow() }()
		_ = ws.Write(r.Context(), websocket.MessageText, []byte(`{"method":"hello"}`))
		for range 3 {
			_, message, err := ws.Read(r.Context())
			if err != nil {
				return
			}
			received <- string(message)
		}
	}
}

func writeChunks(t *testing.T, w io.Writer, chunks ...string) {
	t.Helper()
	for _, chunk := range chunks {
		if _, err := io.WriteString(w, chunk); err != nil {
			t.Fatalf("write %q: %v", chunk, err)
		}
	}
}

func requireMessages(t *testing.T, received <-chan string, want ...string) {
	t.Helper()
	for _, message := range want {
		if got := <-received; got != message {
			t.Fatalf("relay message %q, want %q", got, message)
		}
	}
}

func TestRelaySendsTheTokenOnlyInTheAuthorizationHeaderAndFramesLines(t *testing.T) {
	files := writeRelayFiles(t)
	received := make(chan string, 3)
	srv, requests := startRelayServer(t, files, tls.VersionTLS13, echoThreeMessages(received))
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	conn, err := files.remotes(strings.Replace(srv.URL, "https://", "wss://", 1), files.cert).Relay(ctx, "work")
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = conn.Close() }()
	request := <-requests
	if request.uri != "/rpc" || request.authorization != "Bearer "+files.bearer {
		t.Fatalf("relay saw %q with Authorization %q, want /rpc with the bearer token", request.uri, request.authorization)
	}
	line, err := bufio.NewReader(conn).ReadString('\n')
	if err != nil || line != "{\"method\":\"hello\"}\n" {
		t.Fatalf("read %q, %v", line, err)
	}
	writeChunks(t, conn, "{\"a\":1}\n{\"b\":2}\n", "{\"c\":", "3}\n")
	requireMessages(t, received, `{"a":1}`, `{"b":2}`, `{"c":3}`)
}

func TestRelayRefusalFailsWithoutRevealingTheToken(t *testing.T) {
	files := writeRelayFiles(t)
	srv, requests := startRelayServer(t, files, tls.VersionTLS13, refuseEveryToken)
	_, err := files.remotes(srv.URL, files.cert).Relay(t.Context(), "work")
	if err == nil {
		t.Fatal("a refused token connected")
	}
	if strings.Contains(err.Error(), files.bearer) || !strings.Contains(err.Error(), "token_file") {
		t.Fatalf("error %q reveals the token or names no fix", err)
	}
	if phase := failurePhase(err); phase != PhaseFailed {
		t.Fatalf("refused token leads to %s, want failed", phase)
	}
	if request := <-requests; strings.Contains(request.uri, files.bearer) {
		t.Fatalf("token in the request URI %q", request.uri)
	}
}

func TestRelayRefusesTLSBelow13AndUntrustedCertificates(t *testing.T) {
	files := writeRelayFiles(t)
	var reached atomic.Int32
	count := func(w http.ResponseWriter, r *http.Request) {
		reached.Add(1)
		refuseEveryToken(w, r)
	}
	legacy, _ := startRelayServer(t, files, tls.VersionTLS12, count)
	if _, err := files.remotes(legacy.URL, files.cert).Relay(t.Context(), "work"); err == nil {
		t.Fatal("a TLS 1.2 relay was dialed")
	}
	modern, _ := startRelayServer(t, files, tls.VersionTLS13, count)
	_, err := files.remotes(modern.URL, "").Relay(t.Context(), "work")
	if err == nil || failurePhase(err) != PhaseFailed {
		t.Fatalf("relay with a certificate outside the system roots and no tls_ca_file = %v, want a failed phase", err)
	}
	if reached.Load() != 0 {
		t.Fatalf("the relay handler ran %d times without a trusted TLS 1.3 session", reached.Load())
	}
}

func TestBoxOperationsRefuseADaemonRemote(t *testing.T) {
	files := writeRelayFiles(t)
	_, err := files.remotes("https://work.example", files.cert).Bridge(t.Context(), "work")
	want := "remote: work is a daemon remote and has no box; connect to it from the desktop client"
	if err == nil || err.Error() != want {
		t.Fatalf("bridge to a daemon remote = %v, want %q", err, want)
	}
}

type lineLog chan string

func (l lineLog) Write(p []byte) (int, error) {
	select {
	case l <- string(p):
	default:
	}
	return len(p), nil
}

func serveDalgonaWeb(t *testing.T, files relayFiles, daemon *fakeDaemon) string {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	log := make(lineLog, 4)
	served := make(chan error, 1)
	cfg := webui.Config{Listen: "127.0.0.1:0", Token: files.bearer, CertFile: files.cert, KeyFile: files.key}
	go func() { served <- webui.Serve(ctx, cfg, daemon.dialPipe, log) }()
	t.Cleanup(func() {
		cancel()
		if err := <-served; err != nil {
			t.Errorf("dalgona web: %v", err)
		}
	})
	select {
	case line := <-log:
		base, _, found := strings.Cut(line, "/#token=")
		if !found {
			t.Fatalf("dalgona web announced %q", line)
		}
		return base
	case <-time.After(10 * time.Second):
		t.Fatal("dalgona web did not announce its URL")
		return ""
	}
}

func awaitPhase(t *testing.T, rec *recorder, id EndpointID, want Phase) PhaseChange {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		if change := rec.last(id); change.Phase == want {
			return change
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("%s is %s (%v), want %s", id, rec.last(id).Phase, rec.last(id).Err, want)
	return PhaseChange{}
}

func TestDaemonRemoteConnectsThroughDalgonaWeb(t *testing.T) {
	files := writeRelayFiles(t)
	daemon := newFakeDaemon()
	defer close(daemon.hold)
	base := serveDalgonaWeb(t, files, daemon)
	cfg := config.Config{Home: files.dir, TLSCAFile: files.cert, Remotes: map[string]config.RemoteSpec{
		"work": {Kind: config.RemoteDaemon, URL: base, TokenFile: files.token},
	}}
	rec := newRecorder()
	set := openSet(t, rec, ConfiguredEndpoints(cfg, Backend{}, NewRemotes(cfg, nil))...)
	if err := set.Connect("work"); err != nil {
		t.Fatal(err)
	}
	connected := awaitPhase(t, rec, "work", PhaseConnected)
	if connected.Native.StreamID != "stream" {
		t.Fatalf("native session = %+v, want the daemon stream", connected.Native)
	}
	var echoed string
	if err := set.Call(t.Context(), connected.Link, "test/echo", "through the relay", &echoed); err != nil || echoed != "through the relay" {
		t.Fatalf("call through the relay = %q, %v", echoed, err)
	}
	daemon.notify(t, "test/relayed")
	deadline := time.Now().Add(10 * time.Second)
	for !strings.Contains(strings.Join(rec.eventLog(), "\n"), "work#1 test/relayed") {
		if time.Now().After(deadline) {
			t.Fatalf("relayed notification missing from %v", rec.eventLog())
		}
		time.Sleep(10 * time.Millisecond)
	}
	if got := rec.stampsOf(EndpointID(files.dir)); len(got) != 0 {
		t.Fatalf("connecting work changed the local endpoint: %v", got)
	}
}
