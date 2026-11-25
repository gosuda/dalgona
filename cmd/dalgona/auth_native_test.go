package main

import (
	"bytes"
	"errors"
	"io"
	"io/fs"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/auth/oauth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
)

type authRig struct {
	t       *testing.T
	home    string
	host    *agent.Host
	service *authService
}

type wireAuthStatus struct {
	TransactionID string     `json:"transactionId"`
	Target        authTarget `json:"target"`
	State         string     `json:"state"`
	Challenge     *struct {
		Kind             string `json:"kind"`
		AuthorizationURL string `json:"authorizationUrl"`
		VerificationURL  string `json:"verificationUrl"`
		UserCode         string `json:"userCode"`
		ExpiresAt        string `json:"expiresAt"`
	} `json:"challenge"`
	Error     string `json:"error"`
	ExpiresAt string `json:"expiresAt"`
}

type wireDescriptors struct {
	Targets []struct {
		Target            authTarget `json:"target"`
		SignedIn          bool       `json:"signedIn"`
		Flows             []string   `json:"flows"`
		UnavailableReason string     `json:"unavailableReason"`
	} `json:"targets"`
}

func newAuthRig(t *testing.T, loopback bool, providers map[string]oauth.Provider) *authRig {
	t.Helper()
	home := t.TempDir()
	t.Setenv("DALGONA_HOME", home)
	host, closeHost, err := app.NewHost(t.Context(), app.Options{Product: config.ProductDalgona})
	if err != nil {
		t.Fatalf("app host: %v", err)
	}
	t.Cleanup(func() { _ = closeHost() })
	service := newAuthService(loopback, false, slog.New(slog.DiscardHandler))
	service.lookup = func(id string) (oauth.Provider, bool) {
		provider, ok := providers[id]
		return provider, ok
	}
	return &authRig{t: t, home: home, host: host, service: service}
}

func (r *authRig) connect() *rpc.Conn {
	r.t.Helper()
	serverConn, clientConn := net.Pipe()
	done := make(chan struct{})
	options := rpcserver.ServeOptions{
		Extensions: r.service.extensions(r.host), OperationExtensions: r.service.operations(r.host),
		OperationResults: r.service.operationResults(),
	}
	go func() {
		defer close(done)
		_ = rpcserver.ServeWith(r.t.Context(), serverConn, r.host, options)
	}()
	peer := rpc.Dial(r.t.Context(), clientConn, quietClient{})
	r.t.Cleanup(func() {
		_ = peer.Close()
		_ = clientConn.Close()
		<-done
	})
	return peer
}

func authOK(t *testing.T, peer *rpc.Conn, method string, params map[string]any, out any) {
	t.Helper()
	if err := peer.Call(t.Context(), method, operationParams(method, params), out); err != nil {
		t.Fatalf("%s %v: %v", method, params, err)
	}
}

func authRefused(t *testing.T, peer *rpc.Conn, method string, params map[string]any, kind rpc.ErrorKind) *rpc.RPCError {
	t.Helper()
	err := peer.Call(t.Context(), method, operationParams(method, params), nil)
	coded, ok := errors.AsType[*rpc.RPCError](err)
	if !ok {
		t.Fatalf("%s %v: error %v, want %s", method, params, err, kind)
	}
	data, native := rpc.ErrorDataOf(coded)
	if !native || data.Kind != kind {
		t.Fatalf("%s %v: error %d %q data %s, want %s", method, params, coded.Code, coded.Message, coded.Data, kind)
	}
	return coded
}

func operationParams(method string, params map[string]any) map[string]any {
	switch method {
	case "dalgona/auth/start", "dalgona/auth/submit", "dalgona/auth/logout", "dalgona/mcp/config/write":
		params["operationId"] = rpc.NewOperationID()
	}
	return params
}

func providerTarget(id string) map[string]any { return map[string]any{"kind": "provider", "id": id} }

func freePort(t *testing.T) int {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	port := listener.Addr().(*net.TCPAddr).Port
	if err := listener.Close(); err != nil {
		t.Fatal(err)
	}
	return port
}

func countingServer(t *testing.T, handler http.HandlerFunc) (*httptest.Server, *atomic.Int32) {
	t.Helper()
	var hits atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		handler(w, r)
	}))
	t.Cleanup(server.Close)
	return server, &hits
}

func storedCredential(t *testing.T, home, id string) (auth.Credential, bool) {
	t.Helper()
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range store.Entries() {
		if entry.ID == id {
			return entry.Credential, true
		}
	}
	return auth.Credential{}, false
}

func TestNativeAPIKeyLoginOpensNoListener(t *testing.T) {
	oauthServer, hits := countingServer(t, func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusTeapot) })
	port := freePort(t)
	r := newAuthRig(t, true, map[string]oauth.Provider{"anthropic": {
		ID: "anthropic", AuthorizeURL: oauthServer.URL + "/authorize", TokenURL: oauthServer.URL + "/token",
		RedirectHost: "127.0.0.1", RedirectPort: port, RedirectPath: "/callback",
	}})
	peer := r.connect()
	var started wireAuthStatus
	authOK(t, peer, "dalgona/auth/start", map[string]any{"target": providerTarget("anthropic"), "flow": "apiKey"}, &started)
	if started.State != "awaitingInput" || started.Challenge == nil || started.Challenge.Kind != "apiKey" || started.Challenge.AuthorizationURL != "" {
		t.Fatalf("API-key start = %+v, want awaitingInput with an apiKey challenge and no authorization URL", started)
	}
	requireAwaitingInputWithoutListener(t, peer, port, started.TransactionID)
	if hits.Load() != 0 {
		t.Fatalf("API-key start sent %d OAuth requests", hits.Load())
	}
	_ = authRefused(t, peer, "dalgona/auth/submit", map[string]any{
		"transactionId": started.TransactionID, "response": map[string]any{"kind": "redirectUrl", "value": "http://127.0.0.1/callback?code=x"},
	}, rpc.ErrorInvalidParams)
	var still wireAuthStatus
	authOK(t, peer, "dalgona/auth/status", map[string]any{"transactionId": started.TransactionID}, &still)
	if still.State != "awaitingInput" {
		t.Fatalf("a refused response moved the login to %s", still.State)
	}
}

func requireAwaitingInputWithoutListener(t *testing.T, peer *rpc.Conn, port int, transactionID string) {
	t.Helper()
	for deadline := time.Now().Add(500 * time.Millisecond); time.Now().Before(deadline); time.Sleep(50 * time.Millisecond) {
		listener, err := net.Listen("tcp", net.JoinHostPort("127.0.0.1", strconv.Itoa(port)))
		if err != nil {
			t.Fatalf("API-key login holds the provider callback port: %v", err)
		}
		_ = listener.Close()
		var current wireAuthStatus
		authOK(t, peer, "dalgona/auth/status", map[string]any{"transactionId": transactionID}, &current)
		if current.State != "awaitingInput" || current.Challenge == nil || current.Challenge.AuthorizationURL != "" {
			t.Fatalf("API-key login status = %+v, want awaitingInput without an authorization URL", current)
		}
	}
}

func TestNativeAPIKeySubmitStoresExactKey(t *testing.T) {
	r := newAuthRig(t, false, nil)
	peer := r.connect()
	var started wireAuthStatus
	authOK(t, peer, "dalgona/auth/start", map[string]any{"target": providerTarget("sandbox0"), "flow": "apiKey"}, &started)
	refused := authRefused(t, peer, "dalgona/auth/submit", map[string]any{
		"transactionId": started.TransactionID, "response": map[string]any{"kind": "apiKey", "value": "first\nsecond"},
	}, rpc.ErrorInvalidParams)
	if refused.Message != "invalid text: response.value: line_break_not_allowed" {
		t.Fatalf("multi-line key message = %q", refused.Message)
	}
	if _, saved := storedCredential(t, r.home, "sandbox0"); saved {
		t.Fatal("a refused key changed the credential store")
	}
	key := " sb0-key-7f3a "
	var done wireAuthStatus
	authOK(t, peer, "dalgona/auth/submit", map[string]any{
		"transactionId": started.TransactionID, "response": map[string]any{"kind": "apiKey", "value": key},
	}, &done)
	if done.State != "succeeded" || done.Challenge != nil {
		t.Fatalf("submit = %+v, want succeeded without a challenge", done)
	}
	credential, saved := storedCredential(t, r.home, "sandbox0")
	if !saved || credential.Key != key {
		t.Fatalf("stored credential = %+v, want the exact submitted key", credential)
	}
	_ = authRefused(t, peer, "dalgona/auth/submit", map[string]any{
		"transactionId": started.TransactionID, "response": map[string]any{"kind": "apiKey", "value": "again"},
	}, rpc.ErrorInvalidParams)
}

func TestNativeAuthRefusesCallbackModeMismatchBeforeStarting(t *testing.T) {
	deviceServer, hits := countingServer(t, func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusTeapot) })
	r := newAuthRig(t, true, map[string]oauth.Provider{"copilot": {ID: "copilot", ClientID: "client", DeviceURL: deviceServer.URL + "/device", TokenURL: deviceServer.URL + "/token"}})
	peer := r.connect()
	cases := []map[string]any{
		{"target": providerTarget("openai"), "flow": "apiKey", "callbackMode": "manualPaste"},
		{"target": providerTarget("openai"), "flow": "apiKey", "callbackMode": nil},
		{"target": providerTarget("copilot"), "flow": "deviceCode"},
		{"target": providerTarget("copilot"), "flow": "deviceCode", "callbackMode": nil},
		{"target": providerTarget("copilot"), "flow": "deviceCode", "callbackMode": "backendLoopback"},
		{"target": providerTarget("copilot"), "flow": "browserCallback", "callbackMode": "backendLoopback"},
		{"target": providerTarget("copilot"), "flow": "magicLink"},
	}
	for _, params := range cases {
		_ = authRefused(t, peer, "dalgona/auth/start", params, rpc.ErrorInvalidParams)
	}
	if hits.Load() != 0 {
		t.Fatalf("refused starts sent %d device requests", hits.Load())
	}
}

func TestNativeDeviceLoginCancelStopsPolling(t *testing.T) {
	var pollStarts atomic.Int32
	deviceServer, _ := countingServer(t, func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		if r.URL.Path == "/device" {
			_, _ = io.WriteString(w, `{"device_code":"device","user_code":"WXYZ-1234","verification_uri":"https://example.test/device","interval":1}`)
			return
		}
		w.WriteHeader(http.StatusBadRequest)
		_, _ = io.WriteString(w, `{"error":"authorization_pending"}`)
	})
	trackDevicePollStarts(t, deviceServer.URL, &pollStarts)
	r := newAuthRig(t, true, map[string]oauth.Provider{"copilot": {ID: "copilot", ClientID: "client", DeviceURL: deviceServer.URL + "/device", TokenURL: deviceServer.URL + "/token"}})
	peer, other := r.connect(), r.connect()
	start := map[string]any{"target": providerTarget("copilot"), "flow": "deviceCode", "callbackMode": "device"}
	var started wireAuthStatus
	authOK(t, peer, "dalgona/auth/start", start, &started)
	if started.State != "waiting" || started.Challenge == nil || started.Challenge.UserCode != "WXYZ-1234" || started.Challenge.VerificationURL != "https://example.test/device" {
		t.Fatalf("device start = %+v", started)
	}
	_ = authRefused(t, other, "dalgona/auth/start", start, rpc.ErrorBusy)
	_ = authRefused(t, other, "dalgona/auth/status", map[string]any{"transactionId": started.TransactionID}, rpc.ErrorNotFound)
	waitForPolls(t, &pollStarts, 1)
	requireDeviceLoginCancelled(t, r, peer, started.TransactionID, &pollStarts)
}

func requireDeviceLoginCancelled(t *testing.T, r *authRig, peer *rpc.Conn, transactionID string, polls *atomic.Int32) {
	t.Helper()
	var cancelled struct {
		Status string `json:"status"`
	}
	authOK(t, peer, "dalgona/auth/cancel", map[string]any{"transactionId": transactionID}, &cancelled)
	if cancelled.Status != "cancelled" {
		t.Fatalf("cancel = %q, want cancelled", cancelled.Status)
	}
	stopped := polls.Load()
	time.Sleep(2500 * time.Millisecond)
	if polls.Load() != stopped {
		t.Fatalf("device polling continued after cancel: %d polls, then %d", stopped, polls.Load())
	}
	var status wireAuthStatus
	authOK(t, peer, "dalgona/auth/status", map[string]any{"transactionId": transactionID}, &status)
	if status.State != "cancelled" || status.Challenge != nil {
		t.Fatalf("status after cancel = %+v", status)
	}
	authOK(t, peer, "dalgona/auth/cancel", map[string]any{"transactionId": transactionID}, &cancelled)
	if cancelled.Status != "completed" {
		t.Fatalf("second cancel = %q, want completed", cancelled.Status)
	}
	if _, saved := storedCredential(t, r.home, "copilot"); saved {
		t.Fatal("a cancelled device login stored a credential")
	}
}

func waitForPolls(t *testing.T, polls *atomic.Int32, want int32) {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for polls.Load() < want {
		if time.Now().After(deadline) {
			t.Fatalf("device polls = %d, want at least %d", polls.Load(), want)
		}
		time.Sleep(50 * time.Millisecond)
	}
}

type devicePollStarter struct {
	base  http.RoundTripper
	host  string
	count *atomic.Int32
}

func (d devicePollStarter) RoundTrip(req *http.Request) (*http.Response, error) {
	if req.Method == http.MethodPost && req.URL.Path == "/token" && req.URL.Host == d.host && req.Context().Err() == nil {
		d.count.Add(1)
	}
	return d.base.RoundTrip(req)
}

func trackDevicePollStarts(t *testing.T, serverURL string, count *atomic.Int32) {
	t.Helper()
	endpoint, err := url.Parse(serverURL)
	if err != nil {
		t.Fatal(err)
	}
	base := http.DefaultTransport
	if base == nil {
		t.Fatal("http default transport unavailable")
	}
	t.Cleanup(func() { http.DefaultTransport = base })
	http.DefaultTransport = devicePollStarter{base: base, host: endpoint.Host, count: count}
}

func TestNativeAuthOffersOnlyReachableCallbacks(t *testing.T) {
	chatgpt := oauth.Provider{ //nolint:gosec // test fixture points at example.test URLs, not real credentials
		ID: "chatgpt", AuthorizeURL: "https://example.test/authorize", TokenURL: "https://example.test/token",
		RedirectHost: "127.0.0.1", RedirectPort: freePort(t), RedirectPath: "/auth/callback", DeviceURL: "https://example.test/device",
	}
	cursor := oauth.Provider{ID: "cursor", AuthorizeURL: "https://example.test/login", PollURL: "https://example.test/poll"}
	providers := map[string]oauth.Provider{"chatgpt": chatgpt, "cursor": cursor}
	want := map[bool]map[string][]string{
		true:  {"chatgpt": {"browserCallback", "manualCode", "deviceCode"}, "cursor": {"cursorDeepLink"}, "openai": {"apiKey"}},
		false: {"chatgpt": {"manualCode", "deviceCode"}, "cursor": {"cursorDeepLink"}, "openai": {"apiKey"}},
	}
	for loopback, flows := range want {
		peer := newAuthRig(t, loopback, providers).connect()
		var listed wireDescriptors
		authOK(t, peer, "dalgona/auth/providers", map[string]any{}, &listed)
		for _, target := range listed.Targets {
			if expected, ok := flows[target.Target.ID]; ok && !slicesEqual(target.Flows, expected) {
				t.Errorf("loopback %v: %s flows = %v, want %v", loopback, target.Target.ID, target.Flows, expected)
			}
		}
		if !loopback {
			_ = authRefused(t, peer, "dalgona/auth/start", map[string]any{"target": providerTarget("chatgpt"), "flow": "browserCallback", "callbackMode": "backendLoopback"}, rpc.ErrorInvalidParams)
		}
	}
}

func slicesEqual(got, want []string) bool {
	if len(got) != len(want) {
		return false
	}
	for i := range got {
		if got[i] != want[i] {
			return false
		}
	}
	return true
}

func TestNativeManualCodeChecksRedirectBeforeExchange(t *testing.T) {
	var exchanged atomic.Value
	tokenServer, _ := countingServer(t, func(w http.ResponseWriter, r *http.Request) {
		if err := r.ParseForm(); err == nil {
			exchanged.Store(r.Form.Get("code"))
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{"access_token":"manual-access","refresh_token":"manual-refresh","expires_in":3600}`)
	})
	r := newAuthRig(t, false, map[string]oauth.Provider{"chatgpt": {
		ID: "chatgpt", AuthorizeURL: "https://example.test/authorize", TokenURL: tokenServer.URL + "/token", ClientID: "client",
		RedirectHost: "127.0.0.1", RedirectPort: freePort(t), RedirectPath: "/auth/callback",
	}})
	peer := r.connect()
	var started wireAuthStatus
	authOK(t, peer, "dalgona/auth/start", map[string]any{"target": providerTarget("chatgpt"), "flow": "manualCode", "callbackMode": "manualPaste"}, &started)
	if started.State != "waiting" || started.Challenge == nil {
		t.Fatalf("manual start = %+v", started)
	}
	advertised, err := url.Parse(started.Challenge.AuthorizationURL)
	if err != nil {
		t.Fatal(err)
	}
	redirect, state := advertised.Query().Get("redirect_uri"), advertised.Query().Get("state")
	submit := func(value string) map[string]any {
		return map[string]any{"transactionId": started.TransactionID, "response": map[string]any{"kind": "redirectUrl", "value": value}}
	}
	_ = authRefused(t, peer, "dalgona/auth/submit", submit("http://198.51.100.7:9/auth/callback?code=good&state="+url.QueryEscape(state)), rpc.ErrorInvalidParams)
	_ = authRefused(t, peer, "dalgona/auth/submit", submit(redirect+"?code=good&state=forged"), rpc.ErrorInvalidParams)
	if exchanged.Load() != nil {
		t.Fatal("a refused redirect reached the token endpoint")
	}
	requireManualCodeAccepted(t, peer, submit, redirect, state)
	requireManualCodeStored(t, r, peer, started.TransactionID, &exchanged)
}

func requireManualCodeAccepted(t *testing.T, peer *rpc.Conn, submit func(string) map[string]any, redirect, state string) {
	t.Helper()
	var accepted wireAuthStatus
	authOK(t, peer, "dalgona/auth/submit", submit(redirect+"?code=good&state="+url.QueryEscape(state)), &accepted)
	if accepted.State != "exchanging" && accepted.State != "succeeded" {
		t.Fatalf("accepted submit = %+v", accepted)
	}
}

func requireManualCodeStored(t *testing.T, r *authRig, peer *rpc.Conn, transactionID string, exchanged *atomic.Value) {
	t.Helper()
	final := awaitAuthState(t, peer, transactionID, "succeeded")
	if final.Error != "" || exchanged.Load() != "good" {
		t.Fatalf("final = %+v exchanged %v", final, exchanged.Load())
	}
	credential, saved := storedCredential(t, r.home, "chatgpt")
	if !saved || credential.Access != "manual-access" || credential.Type != "oauth" {
		t.Fatalf("stored credential = %+v", credential)
	}
}

func awaitAuthState(t *testing.T, peer *rpc.Conn, id, want string) wireAuthStatus {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for {
		var status wireAuthStatus
		authOK(t, peer, "dalgona/auth/status", map[string]any{"transactionId": id}, &status)
		if status.State == want {
			return status
		}
		if time.Now().After(deadline) {
			t.Fatalf("login state = %+v, want %s", status, want)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func TestNativeAuthLogoutAndReporterGate(t *testing.T) {
	r := newAuthRig(t, true, nil)
	r.service.homeOverridden = true
	peer := r.connect()
	var started wireAuthStatus
	authOK(t, peer, "dalgona/auth/start", map[string]any{"target": providerTarget("sprites"), "flow": "apiKey"}, &started)
	authOK(t, peer, "dalgona/auth/submit", map[string]any{
		"transactionId": started.TransactionID, "response": map[string]any{"kind": "apiKey", "value": "sprites-key"},
	}, &started)
	var out struct {
		SignedIn *bool `json:"signedIn"`
	}
	authOK(t, peer, "dalgona/auth/logout", map[string]any{"target": providerTarget("sprites")}, &out)
	if out.SignedIn == nil || *out.SignedIn {
		t.Fatalf("logout = %+v, want signedIn false", out)
	}
	if _, saved := storedCredential(t, r.home, "sprites"); saved {
		t.Fatal("logout left the sprites credential")
	}
	reporter := map[string]any{"kind": "reporter", "id": "failure-reporting"}
	refused := authRefused(t, peer, "dalgona/auth/start", map[string]any{"target": reporter, "flow": "deviceCode", "callbackMode": "device"}, rpc.ErrorInvalidParams)
	if refused.Message != "Failure reporting login is unavailable: failure reporting is off for an overridden home" {
		t.Fatalf("reporter refusal = %q", refused.Message)
	}
}

func TestAuthChallengeRemainsOnlyOnTheOwningConnection(t *testing.T) {
	base := "https://example.test"
	r := newAuthRig(t, false, map[string]oauth.Provider{"anthropic": {
		ID: "anthropic", AuthorizeURL: base + "/authorize", TokenURL: base + "/token",
		ClientID: "client", RedirectHost: "127.0.0.1", RedirectPort: freePort(t), RedirectPath: "/callback",
	}})
	peer, other := r.connect(), r.connect()
	id := rpc.NewOperationID()
	params := map[string]any{"target": providerTarget("anthropic"), "flow": "manualCode", "callbackMode": "manualPaste", "operationId": id}
	first := requireAuthStart(t, peer, params)
	if first.Challenge == nil || first.Challenge.AuthorizationURL == "" {
		t.Fatal("owner did not receive the authorization challenge")
	}
	requireNoStoredOAuthState(t, r.home, first.Challenge.AuthorizationURL)
	var receipt struct {
		State  string         `json:"state"`
		Result wireAuthStatus `json:"result"`
	}
	authOK(t, peer, "dalgona/operation/read", map[string]any{"operationId": id}, &receipt)
	if receipt.State != "succeeded" || receipt.Result.TransactionID != first.TransactionID || receipt.Result.Challenge != nil {
		t.Fatal("the durable receipt lost the transaction identity or included its challenge")
	}
	requireAuthReplayPrivacy(t, first, requireAuthStart(t, peer, params), requireAuthStart(t, other, params))
}

func requireAuthStart(t *testing.T, peer *rpc.Conn, params map[string]any) wireAuthStatus {
	t.Helper()
	var status wireAuthStatus
	if err := peer.Call(t.Context(), "dalgona/auth/start", params, &status); err != nil {
		t.Fatal(err)
	}
	return status
}

func requireAuthReplayPrivacy(t *testing.T, first, replay, other wireAuthStatus) {
	t.Helper()
	if replay.TransactionID != first.TransactionID || replay.Challenge == nil || *replay.Challenge != *first.Challenge {
		t.Fatal("the owning connection did not get its original live challenge on replay")
	}
	if other.TransactionID != first.TransactionID || other.Challenge != nil {
		t.Fatal("replay on another connection exposed a live challenge or started a different transaction")
	}
}

func requireNoStoredOAuthState(t *testing.T, home, authorizationURL string) {
	t.Helper()
	state := oauthStateParameter(t, authorizationURL)
	root, err := os.OpenRoot(home)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = root.Close() }()
	err = fs.WalkDir(root.FS(), ".", func(path string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil || !entry.Type().IsRegular() || entry.Name() == "daemon.lock" {
			return walkErr
		}
		raw, readErr := root.ReadFile(path)
		if readErr != nil {
			return readErr
		}
		if bytes.Contains(raw, state) {
			t.Errorf("the authorization challenge was written to %s", filepath.Base(path))
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}

func oauthStateParameter(t *testing.T, authorizationURL string) []byte {
	t.Helper()
	advertised, err := url.Parse(authorizationURL)
	if err != nil {
		t.Fatal("invalid authorization challenge")
	}
	state := []byte(advertised.Query().Get("state"))
	if len(state) == 0 {
		t.Fatal("authorization challenge has no state")
	}
	return state
}
