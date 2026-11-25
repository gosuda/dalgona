//go:build gui

package manage

import (
	"strings"
	"sync"
	"testing"

	"gioui.org/io/semantic"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
)

type mcpFixture struct {
	mu       sync.Mutex
	versions []mcpInventory
	reads    int
	statuses map[string]mcpStatusPage
}

func (m *mcpFixture) read(call) (any, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	inv := m.versions[min(m.reads, len(m.versions)-1)]
	m.reads++
	return inv, nil
}

func (m *mcpFixture) status(c call) (any, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	var p mcpStatusParams
	if err := decodeParams(c, &p); err != nil {
		return nil, err
	}
	page, ok := m.statuses[p.ConfigVersion]
	if !ok {
		return nil, rpc.NativeKind(rpc.ErrorStaleRevision)
	}
	return page, nil
}

func serversRig(t *testing.T, fixture *mcpFixture) (*rig, *Servers) {
	r := newRig(t)
	r.daemon.thread(remoteThread, "Remote name", "", "/work/remote")
	r.connect(localEndpoint, remoteEndpoint)
	r.open(remoteThread)
	r.thread = remoteThread
	r.daemon.onAt(remoteEndpoint, methodMCPConfigRead, fixture.read)
	r.daemon.onAt(remoteEndpoint, methodMCPStatus, fixture.status)
	p := r.panels.Servers
	p.Opened(shell.Route{Panel: "mcp", Thread: remoteThread})
	r.settle(p, &p.work)
	r.settle(p, &p.work)
	return r, p
}

func alphaInventory(version string) mcpInventory {
	return mcpInventory{Version: version, Servers: []mcpServer{
		{Name: "alpha", Source: "user", Editable: true, Effective: true, Config: mcpConfig{
			Kind: "stdio", Command: "alpha-mcp", Args: []string{"--fast"},
			Env: map[string]mcpEnvValue{"TOKEN": {Redacted: true}, "MODE": {Redacted: true}},
		}},
		{Name: "gamma", Source: "project", Effective: true, Config: mcpConfig{Kind: "stdio", Command: "gamma"}},
		{Name: "gamma", Source: "user", Editable: true, Config: mcpConfig{Kind: "stdio", Command: "gamma-user"}},
	}}
}

func (r *rig) count(fragment string) int {
	n := 0
	for _, node := range r.nodes {
		if strings.Contains(node.Desc.Label, fragment) {
			n++
		}
	}
	return n
}

func (r *rig) countButtons(label string) int {
	n := 0
	for _, node := range r.nodes {
		if node.Desc.Class == semantic.Button && node.Desc.Label == label {
			n++
		}
	}
	return n
}

func TestServersMatchRuntimeRowsByExactNameAndSourceOfTheShownVersion(t *testing.T) {
	fixture := &mcpFixture{
		versions: []mcpInventory{alphaInventory("v1")},
		statuses: map[string]mcpStatusPage{"v1": {ConfigVersion: "v1", Data: []mcpRuntime{
			{Name: "alpha", Source: "project", RuntimeStatus: runtimeConnected, Tools: map[string]mcpTool{"a": {}, "b": {}}},
			{Name: "gamma", Source: "project", RuntimeStatus: runtimeConnected, Tools: map[string]mcpTool{"g": {}}},
		}}},
	}
	r, _ := serversRig(t, fixture)
	statuses := r.daemon.sent(methodMCPStatus)
	if len(statuses) != 1 || statuses[0].link.Endpoint != remoteEndpoint {
		t.Fatalf("runtime status calls %+v; want one on builder", statuses)
	}
	if got := params[mcpStatusParams](t, statuses[0]); got.ThreadID != "t1" || got.ConfigVersion != "v1" {
		t.Fatalf("runtime status params %+v; want thread t1 and the displayed version v1", got)
	}
	if n := r.count("Connected with 1 tools."); n != 1 {
		t.Fatalf("the effective project gamma must show its runtime once, shown %d times", n)
	}
	r.mustHide("Connected with 2 tools.")
	r.mustShow("Not started.")
	r.mustShow("Shadowed by an earlier entry with the same name; it does not run.")
	r.mustShow("workspace configuration, read-only here")
	if _, ok := r.find("Edit alpha"); !ok {
		t.Fatal("a user server must offer Edit")
	}
	if n := r.countButtons("Edit gamma"); n != 1 {
		t.Fatalf("only the user gamma entry is editable; Edit gamma shown %d times", n)
	}
}

func TestServersReloadTheInventoryWhenStatusAnswersAnotherVersion(t *testing.T) {
	fixture := &mcpFixture{
		versions: []mcpInventory{alphaInventory("v1"), alphaInventory("v2")},
		statuses: map[string]mcpStatusPage{
			"v1": {ConfigVersion: "v0", Data: []mcpRuntime{{Name: "alpha", Source: "user", RuntimeStatus: runtimeFailed, ToolsError: "old"}}},
			"v2": {ConfigVersion: "v2", Data: []mcpRuntime{{Name: "alpha", Source: "user", RuntimeStatus: runtimeConnected}}},
		},
	}
	r, p := serversRig(t, fixture)
	r.settle(p, &p.work)
	if n := len(r.daemon.sent(methodMCPConfigRead)); n != 2 {
		t.Fatalf("inventory read %d times; another version must reload it once", n)
	}
	statuses := r.daemon.sent(methodMCPStatus)
	if got := params[mcpStatusParams](t, statuses[len(statuses)-1]).ConfigVersion; got != "v2" {
		t.Fatalf("status after reload asked for version %q, want v2", got)
	}
	r.mustShow("Connected with 0 tools.")
	r.mustHide("Failed: old")
}

func TestServersReportMissingProvenanceAsIncompatible(t *testing.T) {
	fixture := &mcpFixture{
		versions: []mcpInventory{alphaInventory("v1")},
		statuses: map[string]mcpStatusPage{"v1": {ConfigVersion: "v1", Data: []mcpRuntime{{Name: "alpha", RuntimeStatus: runtimeConnected}}}},
	}
	r, _ := serversRig(t, fixture)
	r.mustShow("without its configuration source")
	r.mustHide("Connected with")
}

func TestServersWritePreservesRedactedValuesAndKeepsEditsOnAConflict(t *testing.T) {
	fixture := &mcpFixture{
		versions: []mcpInventory{alphaInventory("v1"), alphaInventory("v2"), alphaInventory("v3")},
		statuses: map[string]mcpStatusPage{
			"v1": {ConfigVersion: "v1"}, "v2": {ConfigVersion: "v2"}, "v3": {ConfigVersion: "v3"},
		},
	}
	r, p := serversRig(t, fixture)
	writes := 0
	r.daemon.onAt(remoteEndpoint, methodMCPConfigWrite, func(call) (any, error) {
		writes++
		if writes == 1 {
			return nil, rpc.NativeKind(rpc.ErrorStaleRevision)
		}
		return mcpWritten{Version: "v3", RestartRequiredServers: []string{"gamma"}}, nil
	})
	r.click(p, "Edit alpha")
	r.frame(p)
	r.typeInto(p, "Command", "alpha-mcp2")
	r.click(p, "Save server")
	r.settle(p, &p.work)
	r.mustShow("The configuration changed on the daemon since it loaded. Your edits are kept below")
	checkConflictKept(t, p)
	r.click(p, "Reload")
	r.settle(p, &p.work)
	r.click(p, "Save server")
	r.settle(p, &p.work)
	checkConfigWrites(t, r.daemon.sent(methodMCPConfigWrite))
	r.mustShow("Saved: alpha.")
	r.mustShow("Restart these workspace servers to use the new configuration: gamma.")
	if p.editor.open {
		t.Fatal("a successful save must close the editor")
	}
}

func checkConflictKept(t *testing.T, p *Servers) {
	t.Helper()
	if !p.editor.open || p.editor.command.text() != "alpha-mcp2" {
		t.Fatalf("a conflict closed the editor or lost the edit: open=%v command=%q", p.editor.open, p.editor.command.text())
	}
}

func checkConfigWrites(t *testing.T, sent []call) {
	t.Helper()
	if len(sent) != 2 {
		t.Fatalf("config/write sent %d times, want 2", len(sent))
	}
	for i, want := range []string{"v1", "v2"} {
		checkConfigWrite(t, i, sent[i], want)
	}
}

func checkConfigWrite(t *testing.T, i int, c call, wantVersion string) {
	t.Helper()
	got := params[mcpWriteParams](t, c)
	if c.link.Endpoint != remoteEndpoint || got.ExpectedVersion != wantVersion || got.OperationID == "" {
		t.Fatalf("write %d went to %s with %+v; want builder, expected version %s and an operation id", i+1, c.link.Endpoint, got, wantVersion)
	}
	checkConfigEdit(t, i, got)
}

func checkConfigEdit(t *testing.T, i int, got mcpWriteParams) {
	t.Helper()
	if len(got.Edits) != 1 || got.Edits[0].Name != "alpha" || got.Edits[0].Action != "put" || got.Edits[0].Config == nil {
		t.Fatalf("write %d edits %+v; want one put of alpha", i+1, got.Edits)
	}
	checkEditedConfig(t, i, got.Edits[0].Config)
}

func checkEditedConfig(t *testing.T, i int, c *mcpConfig) {
	t.Helper()
	if c.Kind != "stdio" || c.Command != "alpha-mcp2" || len(c.Args) != 1 || c.Args[0] != "--fast" {
		t.Fatalf("write %d config %+v; want the edited stdio command and the loaded arguments", i+1, c)
	}
	for _, key := range []string{"TOKEN", "MODE"} {
		checkPreserved(t, i, key, c.Env[key])
	}
}

func checkPreserved(t *testing.T, i int, key string, v mcpEnvValue) {
	t.Helper()
	if !v.Preserve || v.Value != nil {
		t.Fatalf("write %d sent %s as %+v; an unchanged redacted value must use the preservation marker", i+1, key, v)
	}
}

func checkProviders(t *testing.T, r *rig) {
	t.Helper()
	providers := r.daemon.sent(methodAuthProviders)
	if len(providers) != 1 || params[threadOnly](t, providers[0]).ThreadID != "t1" {
		t.Fatalf("auth/providers calls %+v; want one for thread t1", providers)
	}
}

func checkAuthStart(t *testing.T, start []call) {
	t.Helper()
	if len(start) != 1 || start[0].link.Endpoint != remoteEndpoint {
		t.Fatalf("auth/start calls %+v; want one on builder", start)
	}
	got := params[authStartParams](t, start[0])
	if got.Target != zetaTarget() || got.Flow != flowManual || got.CallbackMode != "manualPaste" || got.OperationID == "" {
		t.Fatalf("auth/start params %+v; want the exact MCP target, manualCode, manualPaste, and an operation id", got)
	}
}

func checkAuthSubmit(t *testing.T, submit []call) {
	t.Helper()
	if len(submit) != 1 {
		t.Fatalf("auth/submit sent %d times, want 1", len(submit))
	}
	sp := params[authSubmitParams](t, submit[0])
	if sp.TransactionID != "tx-1" || sp.Response != (authResponse{Kind: "authorizationCode", Value: "code-123"}) || sp.OperationID == "" {
		t.Fatalf("auth/submit params %+v; want tx-1 and the exact authorization code", sp)
	}
}

func signInFixture() *mcpFixture {
	inv := mcpInventory{Version: "v1", Servers: []mcpServer{{Name: "zeta", Source: "user", Editable: true, Effective: true, Config: mcpConfig{Kind: "http", URL: "https://mcp.example/zeta", OAuth: true}}}}
	return &mcpFixture{versions: []mcpInventory{inv}, statuses: map[string]mcpStatusPage{"v1": {ConfigVersion: "v1", Data: []mcpRuntime{
		{Name: "zeta", Source: "user", AuthStatus: authNotLoggedIn, RuntimeStatus: runtimeStarting},
	}}}}
}

func zetaTarget() authTarget { return authTarget{Kind: "mcp", Name: "zeta", ThreadID: "t1"} }

func (r *rig) mcpProviders(signedIn func() bool) {
	r.daemon.onAt(remoteEndpoint, methodAuthProviders, func(call) (any, error) {
		return authProviders{Targets: []authDescriptor{
			{Target: authTarget{Kind: "provider", ID: "zeta"}, DisplayName: "Provider zeta", Flows: []string{flowAPIKey}},
			{Target: zetaTarget(), DisplayName: "zeta", SignedIn: signedIn(), AccountLabel: "ops@example", Flows: []string{flowBrowser, flowManual, flowDevice}},
		}}, nil
	})
}

func TestServerSignInUsesTheExactMCPTargetAndAReachableFlow(t *testing.T) {
	r, p := serversRig(t, signInFixture())
	r.mustShow("Authentication required.")
	done := false
	r.mcpProviders(func() bool { return done })
	r.daemon.onAt(remoteEndpoint, methodAuthStart, func(call) (any, error) {
		return authStatus{TransactionID: "tx-1", Target: zetaTarget(), State: "waiting", Challenge: &authChallenge{Kind: "manual", AuthorizationURL: "https://idp.example/authorize"}}, nil
	})
	r.daemon.onAt(remoteEndpoint, methodAuthStatus, func(call) (any, error) {
		return authStatus{TransactionID: "tx-1", Target: zetaTarget(), State: "awaitingInput", Challenge: &authChallenge{Kind: "manual", AuthorizationURL: "https://idp.example/authorize"}}, nil
	})
	r.daemon.onAt(remoteEndpoint, methodAuthSubmit, func(call) (any, error) {
		done = true
		return authStatus{TransactionID: "tx-1", Target: zetaTarget(), State: "succeeded"}, nil
	})
	r.click(p, "Sign-in for zeta")
	r.settle(p, &p.work)
	checkProviders(t, r)
	r.mustShow("zeta: not signed in.")
	r.mustHide("Sign in with Browser sign-in")
	r.click(p, "Sign in with Paste the redirect or code")
	r.settle(p, &p.work)
	checkAuthStart(t, r.daemon.sent(methodAuthStart))
	r.mustShow("https://idp.example/authorize")
	r.typeInto(p, "Redirect URL or authorization code", "code-123")
	r.click(p, "Submit Redirect URL or authorization code")
	r.settle(p, &p.work)
	checkAuthSubmit(t, r.daemon.sent(methodAuthSubmit))
	if p.auth.input.text() != "" {
		t.Fatal("the code field must clear when the flow ends")
	}
	if n := len(r.daemon.sent(methodAuthProviders)); n != 2 {
		t.Fatalf("success must reload the sign-in state once; providers read %d times", n)
	}
	r.mustShow("zeta: signed in as ops@example.")
}

func TestServerSignInCancelSendsTheTransaction(t *testing.T) {
	r, p := serversRig(t, signInFixture())
	r.mcpProviders(func() bool { return false })
	r.daemon.onAt(remoteEndpoint, methodAuthStart, func(call) (any, error) {
		return authStatus{TransactionID: "tx-7", Target: zetaTarget(), State: "awaitingInput", Challenge: &authChallenge{Kind: "manual"}}, nil
	})
	r.daemon.onAt(remoteEndpoint, methodAuthCancel, func(call) (any, error) {
		return struct {
			Status string `json:"status"`
		}{Status: "cancelled"}, nil
	})
	r.click(p, "Sign-in for zeta")
	r.settle(p, &p.work)
	r.click(p, "Sign in with Paste the redirect or code")
	r.settle(p, &p.work)
	r.click(p, "Cancel sign-in")
	r.settle(p, &p.work)
	cancels := r.daemon.sent(methodAuthCancel)
	if len(cancels) != 1 || cancels[0].link.Endpoint != remoteEndpoint || params[authTransaction](t, cancels[0]).TransactionID != "tx-7" {
		t.Fatalf("auth/cancel calls %+v; want tx-7 on builder", cancels)
	}
	r.mustShow("Sign-in cancelled; no credential changed.")
	if n := len(r.daemon.sent(methodAuthSubmit)); n != 0 {
		t.Fatalf("a cancelled sign-in submitted %d responses", n)
	}
}
