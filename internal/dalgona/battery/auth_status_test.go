package battery_test

import (
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/auth/oauth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/battery"
	"github.com/gosuda/dalgona/internal/mcp"
)

func newTokenMCPServer(t *testing.T, wantToken string) (*httptest.Server, *atomic.Int32) {
	t.Helper()
	var requests atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		requests.Add(1)
		if request.Method != http.MethodPost {
			writer.WriteHeader(http.StatusOK)
			return
		}
		if request.Header.Get("Authorization") != "Bearer "+wantToken {
			writer.WriteHeader(http.StatusUnauthorized)
			return
		}
		body, err := io.ReadAll(request.Body)
		if err != nil {
			writer.WriteHeader(http.StatusBadRequest)
			return
		}
		writeMCPReply(t, writer, string(body))
	}))
	t.Cleanup(server.Close)
	return server, &requests
}

func writeMCPReply(t *testing.T, writer http.ResponseWriter, body string) {
	t.Helper()
	identifier := ""
	if _, rest, found := strings.Cut(body, `"id":`); found {
		identifier, _, _ = strings.Cut(rest, ",")
		identifier = strings.TrimSuffix(identifier, "}")
	}
	switch {
	case strings.Contains(body, `"method":"initialize"`):
		writer.Header().Set("Content-Type", "application/json")
		_, _ = fmt.Fprintf(writer, `{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"status","version":"1"}}}`, identifier)
	case strings.Contains(body, `"method":"tools/list"`):
		writer.Header().Set("Content-Type", "application/json")
		_, _ = fmt.Fprintf(writer, `{"jsonrpc":"2.0","id":%s,"result":{"tools":[]}}`, identifier)
	default:
		writer.WriteHeader(http.StatusAccepted)
	}
}

func TestAuthStatusMapping(t *testing.T) {
	bearerServer, _ := newTokenMCPServer(t, "bearer-secret")
	unsetServer, unsetRequests := newTokenMCPServer(t, "never")
	loginServer, loginRequests := newTokenMCPServer(t, "never")
	boundServer, _ := newTokenMCPServer(t, "stored-access")
	t.Setenv("MCP_STATUS_TEST_TOKEN", "bearer-secret")
	t.Setenv("MCP_STATUS_TEST_UNSET", "")
	missing := filepath.Join(t.TempDir(), "no-such-server")
	body := fmt.Sprintf(`[mcp_servers.stdio]
command = %s

[mcp_servers.bearer]
url = %s
bearer_token_env = "MCP_STATUS_TEST_TOKEN"

[mcp_servers.unsetbearer]
url = %s
bearer_token_env = "MCP_STATUS_TEST_UNSET"

[mcp_servers.needslogin]
url = %s
oauth = true

[mcp_servers.loggedin]
url = %s
oauth = true
`, strconv.Quote(missing), strconv.Quote(bearerServer.URL+"/mcp"), strconv.Quote(unsetServer.URL+"/mcp"),
		strconv.Quote(loginServer.URL+"/mcp"), strconv.Quote(boundServer.URL+"/mcp"))
	home, _ := trustEnv(t, body)
	cfg, err := config.Load(config.ProductDalgona, t.TempDir(), config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	store := bindLoggedInServer(t, home, boundServer.URL+"/mcp")
	extra, err := (&battery.Daemon{}).Load(t.Context(), app.Base{Config: cfg, Auth: store})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := extra.Close(); closeErr != nil {
			t.Error(closeErr)
		}
	})
	project := t.TempDir()
	extra.Tools(t.Context(), project)
	got := map[string][3]string{}
	for _, status := range extra.MCPStatus(t.Context(), project) {
		got[status.Name] = [3]string{status.AuthStatus, status.Status, status.Error}
	}
	checkStatusRow(t, got, statusRow{"stdio", "unsupported", "failed", ""})
	checkStatusRow(t, got, statusRow{"bearer", "bearerToken", "connected", ""})
	checkStatusRow(t, got, statusRow{"unsetbearer", "notLoggedIn", "failed", "bearer_token_env MCP_STATUS_TEST_UNSET is not set"})
	checkStatusRow(t, got, statusRow{"needslogin", "notLoggedIn", "authenticationRequired", "login required: run dalgona mcp login needslogin"})
	checkStatusRow(t, got, statusRow{"loggedin", "oAuth", "connected", ""})
	if requests := unsetRequests.Load() + loginRequests.Load(); requests != 0 {
		t.Fatalf("servers without a usable credential received %d requests, want none", requests)
	}
}

func bindLoggedInServer(t *testing.T, home, mcpURL string) *auth.Store {
	t.Helper()
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	resource, err := oauth.CanonicalResource(mcpURL)
	if err != nil {
		t.Fatal(err)
	}
	registration := fmt.Sprintf(`{"loggedin":{"resource":%q,"issuer":"http://127.0.0.1:1","authorization_endpoint":"http://127.0.0.1:1/authorize","token_endpoint":"http://127.0.0.1:1/token","client_id":"client-1"}}`, resource)
	if writeErr := os.WriteFile(filepath.Join(home, mcp.RegistrationFile), []byte(registration), 0o600); writeErr != nil {
		t.Fatal(writeErr)
	}
	credential := auth.Credential{
		Type: "oauth", Access: "stored-access", Refresh: "refresh", Account: resource,
		Expires: time.Now().Add(time.Hour).UTC().Format(time.RFC3339),
	}
	if setErr := store.SetCredential(mcp.CredentialKey("loggedin"), credential); setErr != nil {
		t.Fatal(setErr)
	}
	return store
}

type statusRow struct {
	name       string
	authStatus string
	runtime    string
	errorText  string
}

func checkStatusRow(t *testing.T, got map[string][3]string, want statusRow) {
	t.Helper()
	row, ok := got[want.name]
	if !ok {
		t.Fatalf("no status for %s in %v", want.name, got)
	}
	if row[0] != want.authStatus || row[1] != want.runtime {
		t.Errorf("%s = authStatus %q, runtimeStatus %q; want %q, %q", want.name, row[0], row[1], want.authStatus, want.runtime)
	}
	if !strings.Contains(row[2], want.errorText) {
		t.Errorf("%s error = %q, want it to contain %q", want.name, row[2], want.errorText)
	}
	if (want.runtime == "connected") != (row[2] == "") {
		t.Errorf("%s error %q does not match runtime status %q", want.name, row[2], want.runtime)
	}
}
