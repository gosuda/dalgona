package rpc

import (
	"context"
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/config"
)

func fixtureMCPAuthStatus(context.Context, string) []agent.MCPServerStatus {
	return []agent.MCPServerStatus{
		{Name: "a-bearer", AuthStatus: "bearerToken", Status: "connected"},
		{Name: "b-oauth", AuthStatus: "oAuth", Status: "connected"},
		{Name: "c-login", AuthStatus: "notLoggedIn", Status: "authenticationRequired", Error: "login required: run dalgona mcp login c-login"},
		{Name: "d-plain", Status: "connected"},
	}
}

func TestMCPServerStatusReportsAuth(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	r.cfg.MCPStatus = fixtureMCPAuthStatus
	r.openHost()
	c := r.connect()
	var all mcpStatusPage
	c.ok("mcpServerStatus/list", map[string]any{}, &all)
	want := []struct{ name, authStatus, runtimeStatus, toolsError string }{
		{"a-bearer", "bearerToken", "connected", ""},
		{"b-oauth", "oAuth", "connected", ""},
		{"c-login", "notLoggedIn", "authenticationRequired", "login required: run dalgona mcp login c-login"},
		{"d-plain", "unsupported", "connected", ""},
	}
	if len(all.Data) != len(want) {
		t.Fatalf("servers = %+v, want %d", all.Data, len(want))
	}
	for i, row := range want {
		got := all.Data[i]
		if got.Name != row.name || got.AuthStatus != row.authStatus || got.RuntimeStatus != row.runtimeStatus || got.ToolsError != row.toolsError {
			t.Errorf("server %d = %+v, want %+v", i, got, row)
		}
	}
}
