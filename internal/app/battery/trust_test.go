package battery_test

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/app"
	"github.com/gosuda/dalgona/internal/app/battery"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/discovery"
	"github.com/gosuda/dalgona/internal/tool"
)

func trustEnv(t *testing.T, configBody string) (dalHome, userHome string) {
	t.Helper()
	dalHome = t.TempDir()
	userHome = t.TempDir()
	t.Setenv("DALGONA_HOME", dalHome)
	t.Setenv("HOME", userHome)
	t.Setenv("USERPROFILE", userHome)
	if configBody != "" {
		if err := os.WriteFile(filepath.Join(dalHome, "config.toml"), []byte(configBody), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	return dalHome, userHome
}

func writeWorkspaceMCP(t *testing.T, cwd, server string) {
	t.Helper()
	doc := fmt.Sprintf(`{"mcpServers":{"wsfix":{"command":%q}}}`, server)
	if err := os.WriteFile(filepath.Join(cwd, ".mcp.json"), []byte(doc), 0o600); err != nil {
		t.Fatal(err)
	}
}

func hasToolName(tools []tool.Tool, name string) bool {
	for _, candidate := range tools {
		if candidate.Spec().Name == name {
			return true
		}
	}
	return false
}

type trustFixture struct {
	dalHome  string
	cwd      string
	startLog string
	cfg      config.Config
}

func newTrustFixture(t *testing.T, server string) trustFixture {
	t.Helper()
	_, _ = trustEnv(t, "[discovery]\nsources = [\"claude\"]\n")
	cwd := t.TempDir()
	writeWorkspaceMCP(t, cwd, server)
	startLog := filepath.Join(t.TempDir(), "starts")
	t.Setenv("START_LOG", startLog)
	cfg, err := config.Load(config.ProductDalgona, cwd, config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	return trustFixture{dalHome: os.Getenv("DALGONA_HOME"), cwd: cwd, startLog: startLog, cfg: cfg}
}

func loadDaemon(t *testing.T, daemon *battery.Daemon, cfg config.Config) app.Batteries {
	t.Helper()
	extra, err := daemon.Load(t.Context(), app.Base{Config: cfg})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := extra.Close(); closeErr != nil {
			t.Error(closeErr)
		}
	})
	return extra
}

func requireWorkspaceEcho(t *testing.T, extra app.Batteries, cwd string) {
	t.Helper()
	if !hasToolName(extra.Tools(t.Context(), cwd), "wsfix__echo") {
		t.Fatal("workspace MCP tool is absent")
	}
}

func requireNoWorkspaceEcho(t *testing.T, extra app.Batteries, cwd string) {
	t.Helper()
	if hasToolName(extra.Tools(t.Context(), cwd), "wsfix__echo") {
		t.Fatal("workspace MCP tool is present")
	}
}

func requireOneStart(t *testing.T, startLog string) {
	t.Helper()
	data, err := os.ReadFile(startLog)
	if err != nil || string(data) != "start\n" {
		t.Fatalf("start log = %q, %v, want one start", data, err)
	}
}

func requireNoStart(t *testing.T, startLog string) {
	t.Helper()
	if data, err := os.ReadFile(startLog); err == nil && len(data) > 0 {
		t.Fatalf("workspace MCP server started: %q", data)
	}
}

type approvalSight struct {
	decision   battery.TrustDecision
	wantServer string
	calls      int
	cwd        string
	single     bool
}

func sightedDaemon(sight *approvalSight) *battery.Daemon {
	return &battery.Daemon{WorkspaceApprover: func(_ context.Context, cwd string, servers []discovery.MCPServer) battery.TrustDecision {
		sight.calls++
		sight.cwd = cwd
		sight.single = len(servers) == 1 && (sight.wantServer == "" || servers[0].Command == sight.wantServer)
		return sight.decision
	}}
}

func TestWorkspaceServerDoesNotStartBeforeTrust(t *testing.T) {
	fixture := newTrustFixture(t, buildMCPServer(t))
	extra := loadDaemon(t, &battery.Daemon{}, fixture.cfg)
	requireNoWorkspaceEcho(t, extra, fixture.cwd)
	requireNoStart(t, fixture.startLog)
	requireNoWorkspaceEcho(t, extra, fixture.cwd)
}

func TestWorkspaceServerStartsAfterTrust(t *testing.T) {
	server := buildMCPServer(t)
	fixture := newTrustFixture(t, server)
	sight := &approvalSight{decision: battery.TrustAccept, wantServer: server}
	extra := loadDaemon(t, sightedDaemon(sight), fixture.cfg)
	requireWorkspaceEcho(t, extra, fixture.cwd)
	wantCwd := config.CanonicalWorkspace(fixture.cwd)
	if sight.calls != 1 || sight.cwd != wantCwd || !sight.single {
		t.Fatalf("approver calls = %d, cwd = %q, single = %v", sight.calls, sight.cwd, sight.single)
	}
	requireOneStart(t, fixture.startLog)
	trusted, err := config.TrustedWorkspaces(fixture.dalHome)
	if err != nil || len(trusted) != 1 || trusted[0] != wantCwd {
		t.Fatalf("trusted workspaces = %q, %v, want [%q]", trusted, err, wantCwd)
	}
	requireWorkspaceEcho(t, extra, fixture.cwd)
	if sight.calls != 1 {
		t.Fatalf("approver calls = %d, want exactly one prompt", sight.calls)
	}
}

func TestWorkspaceTrustPersistsAcrossRestart(t *testing.T) {
	fixture := newTrustFixture(t, buildMCPServer(t))
	first := loadDaemon(t, sightedDaemon(&approvalSight{decision: battery.TrustAccept}), fixture.cfg)
	requireWorkspaceEcho(t, first, fixture.cwd)
	reloaded, err := config.Load(config.ProductDalgona, fixture.cwd, config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	quiet := loadDaemon(t, &battery.Daemon{}, reloaded)
	requireWorkspaceEcho(t, quiet, fixture.cwd)
}

func TestWorkspaceRefusalHoldsForSession(t *testing.T) {
	fixture := newTrustFixture(t, buildMCPServer(t))
	sight := &approvalSight{}
	extra := loadDaemon(t, sightedDaemon(sight), fixture.cfg)
	requireNoWorkspaceEcho(t, extra, fixture.cwd)
	requireNoWorkspaceEcho(t, extra, fixture.cwd)
	if sight.calls != 1 {
		t.Fatalf("approver calls = %d, want exactly one prompt", sight.calls)
	}
	requireNoStart(t, fixture.startLog)
}

func TestWorkspaceCancelPromptsAgain(t *testing.T) {
	fixture := newTrustFixture(t, buildMCPServer(t))
	sight := &approvalSight{decision: battery.TrustCancel}
	extra := loadDaemon(t, sightedDaemon(sight), fixture.cfg)
	requireNoWorkspaceEcho(t, extra, fixture.cwd)
	requireNoWorkspaceEcho(t, extra, fixture.cwd)
	if sight.calls != 2 {
		t.Fatalf("approver calls = %d, want a prompt per turn after cancel", sight.calls)
	}
	requireNoStart(t, fixture.startLog)
	trusted, err := config.TrustedWorkspaces(fixture.dalHome)
	if err != nil || len(trusted) != 0 {
		t.Fatalf("trusted workspaces = %q, %v, want none after cancel", trusted, err)
	}
}

func TestHomeDiscoveredServersStartWithoutTrust(t *testing.T) {
	server := buildMCPServer(t)
	_, userHome := trustEnv(t, "[discovery]\nsources = [\"claude\"]\n")
	doc := fmt.Sprintf(`{"mcpServers":{"homefix":{"command":%q}}}`, server)
	if err := os.MkdirAll(filepath.Join(userHome, ".claude"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(userHome, ".claude", "mcp.json"), []byte(doc), 0o600); err != nil {
		t.Fatal(err)
	}
	cwd := t.TempDir()
	t.Setenv("START_LOG", filepath.Join(t.TempDir(), "starts"))
	cfg, err := config.Load(config.ProductDalgona, cwd, config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	extra := loadDaemon(t, &battery.Daemon{}, cfg)
	if !hasToolName(extra.Tools(t.Context(), cwd), "homefix__echo") {
		t.Fatal("home MCP tool is absent without any trust prompt")
	}
}

func TestWorkspaceTrustPromptShowsCommands(t *testing.T) {
	servers := []discovery.MCPServer{
		{Name: "beta", Command: "uvx", Args: []string{"mcp-server", "--fast"}, Workspace: true},
		{Name: "alpha", URL: "http://127.0.0.1:9000/mcp", Workspace: true},
	}
	prompt := battery.WorkspaceTrustPrompt("/work/repo", servers)
	for _, want := range []string{"/work/repo", "beta: uvx mcp-server --fast", "alpha: http://127.0.0.1:9000/mcp"} {
		if !strings.Contains(prompt, want) {
			t.Fatalf("trust prompt %q does not contain %q", prompt, want)
		}
	}
}
