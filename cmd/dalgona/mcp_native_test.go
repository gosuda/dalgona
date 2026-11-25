package main

import (
	"bytes"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/battery"
)

type mcpRig struct {
	t       *testing.T
	home    string
	host    *agent.Host
	service *mcpConfigService
}

type wireMCPConfig struct {
	Version          string `json:"version"`
	TrustedWorkspace bool   `json:"trustedWorkspace"`
	Servers          []struct {
		Name      string         `json:"name"`
		Source    string         `json:"source"`
		Editable  bool           `json:"editable"`
		Effective bool           `json:"effective"`
		Config    jsontext.Value `json:"config"`
	} `json:"servers"`
}

type wireMCPStatus struct {
	Data []struct {
		Name          string `json:"name"`
		Source        string `json:"source"`
		RuntimeStatus string `json:"runtimeStatus"`
	} `json:"data"`
	NextCursor    string `json:"nextCursor"`
	ConfigVersion string `json:"configVersion"`
}

func newMCPRig(t *testing.T, configBody string, files map[string]string) *mcpRig {
	t.Helper()
	home, userHome, cwd := t.TempDir(), t.TempDir(), t.TempDir()
	t.Setenv("DALGONA_HOME", home)
	t.Setenv("HOME", userHome)
	t.Setenv("USERPROFILE", userHome)
	files[filepath.Join(home, "config.toml")] = configBody
	for path, body := range files {
		path = strings.NewReplacer("$HOME", userHome, "$CWD", cwd).Replace(path)
		if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	cfg, err := config.Load(config.ProductDalgona, cwd, config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	batteries := &battery.Daemon{}
	host, closeHost, err := app.NewHost(t.Context(), app.Options{
		Product: config.ProductDalgona, Config: &cfg, Logger: slog.New(slog.DiscardHandler), Batteries: batteries.Load,
	})
	if err != nil {
		t.Fatalf("app host: %v", err)
	}
	t.Cleanup(func() { _ = closeHost() })
	return &mcpRig{t: t, home: home, host: host, service: newMCPConfigService(batteries)}
}

func (r *mcpRig) connect(native bool) *rpc.Conn {
	r.t.Helper()
	serverConn, clientConn := net.Pipe()
	done := make(chan struct{})
	options := rpcserver.ServeOptions{Extensions: r.service.extensions(r.host), OperationExtensions: r.service.operations(r.host)}
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
	params := map[string]any{"clientInfo": map[string]string{"name": "native-test", "version": "1"}}
	if native {
		params["capabilities"] = map[string]any{"dalgonaNative": map[string]any{"version": rpc.NativeVersion, "delivery": rpc.NativeDelivery}}
	}
	authOK(r.t, peer, "initialize", params, nil)
	return peer
}

func (r *mcpRig) settings() []byte {
	r.t.Helper()
	raw, err := os.ReadFile(filepath.Join(r.home, "settings.toml"))
	if err != nil && !os.IsNotExist(err) {
		r.t.Fatal(err)
	}
	return raw
}

const sameNameConfig = `[discovery]
sources = ["claude"]

[mcp_servers.docs]
command = "/nonexistent/docs-user"
env = { DOCS_TOKEN = "user-secret-7f3a" }
`

func sameNameFiles() map[string]string {
	return map[string]string{
		"$HOME/.claude.json": `{"mcpServers":{"docs":{"command":"/nonexistent/docs-discovered"},"wiki":{"command":"/nonexistent/wiki"}}}`,
		"$CWD/.mcp.json":     `{"mcpServers":{"docs":{"command":"/nonexistent/docs-project"}}}`,
	}
}

func TestNativeMCPInventoryKeepsProvenanceForSameNames(t *testing.T) {
	r := newMCPRig(t, sameNameConfig, sameNameFiles())
	peer := r.connect(true)
	var raw jsontext.Value
	authOK(t, peer, "dalgona/mcp/config/read", map[string]any{}, &raw)
	inventory := requireMCPInventoryShape(t, raw)
	requireMCPInventoryRows(t, inventory)
	requireNativeStatusRow(t, peer, inventory)
	requireLegacyStatusHidden(t, r)
}

func requireMCPInventoryShape(t *testing.T, raw jsontext.Value) wireMCPConfig {
	t.Helper()
	if bytes.Contains(raw, []byte("user-secret-7f3a")) {
		t.Fatalf("config/read returned a secret environment value: %s", raw)
	}
	var inventory wireMCPConfig
	if err := json.Unmarshal(raw, &inventory); err != nil {
		t.Fatal(err)
	}
	if len(inventory.Servers) != 4 || inventory.Version == "" {
		t.Fatalf("inventory = %s", raw)
	}
	return inventory
}

func requireMCPInventoryRows(t *testing.T, inventory wireMCPConfig) {
	t.Helper()
	want := []struct {
		name, source        string
		editable, effective bool
	}{
		{"docs", "user", true, true}, {"docs", "discovered", false, false}, {"wiki", "discovered", false, true}, {"docs", "project", false, false},
	}
	for i, row := range want {
		got := inventory.Servers[i]
		if got.Name != row.name || got.Source != row.source || got.Editable != row.editable || got.Effective != row.effective {
			t.Errorf("row %d = %+v, want %+v", i, got, row)
		}
	}
	if !strings.Contains(string(inventory.Servers[0].Config), `"DOCS_TOKEN":{"redacted":true}`) {
		t.Errorf("user docs config = %s, want a redacted DOCS_TOKEN", inventory.Servers[0].Config)
	}
}

func requireNativeStatusRow(t *testing.T, peer *rpc.Conn, inventory wireMCPConfig) {
	t.Helper()
	var status wireMCPStatus
	authOK(t, peer, "mcpServerStatus/list", map[string]any{"configVersion": inventory.Version}, &status)
	if status.ConfigVersion != inventory.Version || len(status.Data) != 1 || status.Data[0].Name != "docs" || status.Data[0].Source != "user" {
		t.Fatalf("native status = %+v, want only the effective user docs row", status)
	}
}

func requireLegacyStatusHidden(t *testing.T, r *mcpRig) {
	t.Helper()
	var legacy jsontext.Value
	authOK(t, r.connect(false), "mcpServerStatus/list", map[string]any{}, &legacy)
	if bytes.Contains(legacy, []byte(`"source"`)) || bytes.Contains(legacy, []byte(`"configVersion"`)) {
		t.Fatalf("a client without dalgonaNative received provenance: %s", legacy)
	}
}

func TestNativeMCPConfigWriteRefusesStaleVersion(t *testing.T) {
	r := newMCPRig(t, sameNameConfig, sameNameFiles())
	peer := r.connect(true)
	var first wireMCPConfig
	authOK(t, peer, "dalgona/mcp/config/read", map[string]any{}, &first)
	_ = authRefused(t, peer, "dalgona/mcp/config/write", map[string]any{
		"expectedVersion": first.Version, "edits": []any{map[string]any{"name": "docs", "action": "remove"}},
	}, rpc.ErrorInvalidParams)
	search := map[string]any{"name": "search", "action": "put", "config": map[string]any{"kind": "http", "url": "https://mcp.example.test/mcp", "oauth": false}}
	version, before := requireSearchServerWrite(t, peer, r, first, search)
	requireStaleRevisionRefused(t, peer, first, search, version)
	requireSettingsUnchanged(t, r, before)
	version = requirePreservedDocsWrite(t, peer, version)
	requireDocsSearchStatus(t, peer, r, version)
}

func requireSearchServerWrite(t *testing.T, peer *rpc.Conn, r *mcpRig, first wireMCPConfig, search map[string]any) (string, []byte) {
	t.Helper()
	var written struct {
		Version                string   `json:"version"`
		RestartRequiredServers []string `json:"restartRequiredServers"`
	}
	authOK(t, peer, "dalgona/mcp/config/write", map[string]any{"expectedVersion": first.Version, "edits": []any{search}}, &written)
	if written.Version == "" || written.Version == first.Version || len(written.RestartRequiredServers) != 0 {
		t.Fatalf("write = %+v, want a new version and no restart", written)
	}
	before := r.settings()
	if !bytes.Contains(before, []byte("https://mcp.example.test/mcp")) {
		t.Fatalf("settings.toml = %s, want the search server", before)
	}
	return written.Version, before
}

func requireStaleRevisionRefused(t *testing.T, peer *rpc.Conn, first wireMCPConfig, search map[string]any, version string) {
	t.Helper()
	refused := authRefused(t, peer, "dalgona/mcp/config/write", map[string]any{"expectedVersion": first.Version, "edits": []any{search}}, rpc.ErrorStaleRevision)
	if data, _ := rpc.ErrorDataOf(refused); data.CurrentRevision != version {
		t.Fatalf("stale write current revision = %q, want %q", data.CurrentRevision, version)
	}
}

func requireSettingsUnchanged(t *testing.T, r *mcpRig, before []byte) {
	t.Helper()
	if !bytes.Equal(r.settings(), before) {
		t.Fatal("a stale write changed settings.toml")
	}
}

func requirePreservedDocsWrite(t *testing.T, peer *rpc.Conn, version string) string {
	t.Helper()
	preserve := map[string]any{"name": "docs", "action": "put", "config": map[string]any{
		"kind": "stdio", "command": "/nonexistent/docs-user-2", "env": map[string]any{"DOCS_TOKEN": map[string]any{"preserve": true}},
	}}
	for _, env := range []map[string]any{{"DOCS_TOKEN": map[string]any{"redacted": true}}, {"OTHER": map[string]any{"preserve": true}}} {
		placeholder := map[string]any{"name": "docs", "action": "put", "config": map[string]any{"kind": "stdio", "command": "/bin/docs", "env": env}}
		_ = authRefused(t, peer, "dalgona/mcp/config/write", map[string]any{"expectedVersion": version, "edits": []any{placeholder}}, rpc.ErrorInvalidParams)
	}
	var written struct {
		Version                string   `json:"version"`
		RestartRequiredServers []string `json:"restartRequiredServers"`
	}
	authOK(t, peer, "dalgona/mcp/config/write", map[string]any{"expectedVersion": version, "edits": []any{preserve}}, &written)
	if len(written.RestartRequiredServers) != 0 {
		t.Fatalf("restart required = %v, want none; the daemon restarts its user servers", written.RestartRequiredServers)
	}
	return written.Version
}

func requireDocsSearchStatus(t *testing.T, peer *rpc.Conn, r *mcpRig, version string) {
	t.Helper()
	var status wireMCPStatus
	authOK(t, peer, "mcpServerStatus/list", map[string]any{"configVersion": version}, &status)
	if len(status.Data) != 2 || status.Data[0].Name != "docs" || status.Data[1].Name != "search" || status.Data[0].Source != "user" {
		t.Fatalf("status after the writes = %+v, want the restarted docs and the started search user rows", status)
	}
	if after := r.settings(); !bytes.Contains(after, []byte("user-secret-7f3a")) || !bytes.Contains(after, []byte("docs-user-2")) {
		t.Fatalf("settings.toml = %s, want the preserved secret beside the new command", after)
	}
}

const pagedConfig = `[mcp_servers.alpha]
command = "/nonexistent/alpha"

[mcp_servers.beta]
command = "/nonexistent/beta"
`

func TestNativeMCPStatusDetectsChangeBetweenPages(t *testing.T) {
	r := newMCPRig(t, pagedConfig, map[string]string{})
	peer := r.connect(true)
	var inventory wireMCPConfig
	authOK(t, peer, "dalgona/mcp/config/read", map[string]any{}, &inventory)
	page, version := requireFirstStatusPage(t, peer, inventory.Version)
	requireStalePageRefused(t, peer, inventory.Version, page, version)
	requireOutsideEditDetected(t, peer, r)
}

func requireFirstStatusPage(t *testing.T, peer *rpc.Conn, configVersion string) (wireMCPStatus, string) {
	t.Helper()
	var page wireMCPStatus
	authOK(t, peer, "mcpServerStatus/list", map[string]any{"configVersion": configVersion, "limit": 1}, &page)
	if len(page.Data) != 1 || page.Data[0].Name != "alpha" || page.NextCursor == "" {
		t.Fatalf("first page = %+v", page)
	}
	replacement := map[string]any{"name": "alpha", "action": "put", "config": map[string]any{"kind": "stdio", "command": "/nonexistent/alpha-2"}}
	var written struct {
		Version string `json:"version"`
	}
	authOK(t, peer, "dalgona/mcp/config/write", map[string]any{"expectedVersion": configVersion, "edits": []any{replacement}}, &written)
	return page, written.Version
}

func requireStalePageRefused(t *testing.T, peer *rpc.Conn, configVersion string, page wireMCPStatus, version string) {
	t.Helper()
	refused := authRefused(t, peer, "mcpServerStatus/list", map[string]any{"configVersion": configVersion, "cursor": page.NextCursor, "limit": 1}, rpc.ErrorStaleRevision)
	if data, _ := rpc.ErrorDataOf(refused); data.CurrentRevision != version {
		t.Fatalf("stale page current revision = %q, want %q", data.CurrentRevision, version)
	}
	_ = authRefused(t, peer, "mcpServerStatus/list", map[string]any{"configVersion": version, "cursor": page.NextCursor}, rpc.ErrorStaleRevision)
	var fresh wireMCPStatus
	authOK(t, peer, "mcpServerStatus/list", map[string]any{"configVersion": version}, &fresh)
	if len(fresh.Data) != 2 || fresh.Data[0].Name != "alpha" || fresh.Data[1].Name != "beta" {
		t.Fatalf("fresh status = %+v, want the restarted alpha and the unchanged beta", fresh)
	}
}

func requireOutsideEditDetected(t *testing.T, peer *rpc.Conn, r *mcpRig) {
	t.Helper()
	outside := append(r.settings(), []byte("\n[mcp_servers.beta]\ncommand = \"/nonexistent/beta-2\"\n")...)
	if err := os.WriteFile(filepath.Join(r.home, "settings.toml"), outside, 0o600); err != nil {
		t.Fatal(err)
	}
	var edited wireMCPConfig
	authOK(t, peer, "dalgona/mcp/config/read", map[string]any{}, &edited)
	refused := authRefused(t, peer, "mcpServerStatus/list", map[string]any{"configVersion": edited.Version}, rpc.ErrorStaleRevision)
	if data, _ := rpc.ErrorDataOf(refused); data.CurrentRevision != edited.Version {
		t.Fatalf("inconsistent runtime current revision = %q, want %q", data.CurrentRevision, edited.Version)
	}
	_ = authRefused(t, peer, "mcpServerStatus/list", map[string]any{}, rpc.ErrorInvalidParams)
}

func TestMCPWriteRejectsSettingsChangedAfterInventoryCheck(t *testing.T) {
	r := newMCPRig(t, pagedConfig, map[string]string{})
	call := mcpConfigCall{service: r.service, host: r.host}
	ctx, cwd := t.Context(), r.host.Config().Cwd
	inventory, err := call.inventory(ctx, cwd)
	if err != nil {
		t.Fatal(err)
	}
	checked, err := call.expected(cwd, inventory.Version)
	if err != nil {
		t.Fatal(err)
	}
	outside := "[mcp_servers.alpha]\ncommand = '/nonexistent/newer'\n"
	if writeErr := os.WriteFile(filepath.Join(r.home, "settings.toml"), []byte(outside), 0o600); writeErr != nil {
		t.Fatal(writeErr)
	}
	edits := []mcpEdit{{Name: "alpha", Action: "put", Config: &mcpEditConfig{Kind: "stdio", Command: "/nonexistent/stale"}}}
	_, err = call.apply(ctx, cwd, checked, edits)
	coded, ok := errors.AsType[*rpc.RPCError](err)
	if !ok {
		t.Fatalf("settings write error = %v, want stale_revision", err)
	}
	if data, _ := rpc.ErrorDataOf(coded); data.Kind != rpc.ErrorStaleRevision || string(r.settings()) != outside {
		t.Fatalf("settings race: got error %s and settings %q, want stale_revision and newer settings", data.Kind, r.settings())
	}
}
