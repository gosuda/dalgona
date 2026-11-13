package rpc

import (
	"context"
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
)

type mcpStatusView struct {
	Name          string `json:"name"`
	AuthStatus    string `json:"authStatus"`
	RuntimeStatus string `json:"runtimeStatus"`
	ToolsError    string `json:"toolsError"`
	Tools         map[string]struct {
		Name        string         `json:"name"`
		Description string         `json:"description"`
		InputSchema jsontext.Value `json:"inputSchema"`
	} `json:"tools"`
}

type mcpStatusPage struct {
	Data       []mcpStatusView `json:"data"`
	NextCursor string          `json:"nextCursor"`
}

func fixtureMCPStatus(context.Context, string) []agent.MCPServerStatus {
	return []agent.MCPServerStatus{
		{Name: "broken", Status: "failed", Error: "exit status 1"},
		{Name: "docs", Status: "connected", Tools: []agent.MCPToolStatus{{Name: "lookup", Spec: tool.Spec{
			Name:        "docs__lookup",
			Description: "Look up a page.",
			Schema:      jsontext.Value(`{"type":"object","properties":{"q":{"type":"string"}}}`),
		}}}},
		{Name: "plug:srv", Status: "connected", Tools: []agent.MCPToolStatus{{Name: "echo", Spec: tool.Spec{
			Name:        "plug_srv__echo_0123abcd",
			Description: "Echo text.",
			Schema:      jsontext.Value(`{"type":"object"}`),
		}}}},
	}
}

func TestMCPServerStatusList(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	r.cfg.MCPStatus = fixtureMCPStatus
	r.openHost()
	c := r.connect()
	checkMCPStatusEntries(t, c)
	checkMCPStatusPages(t, c)
	c.fails("mcpServerStatus/list", map[string]any{"limit": 1001}, -32602, "limit must be at most 1000")
	c.fails("mcpServerStatus/list", map[string]any{"threadId": "missing"}, -32602, "unknown thread missing")
}

func checkMCPStatusEntries(t *testing.T, c *wireClient) {
	t.Helper()
	var all mcpStatusPage
	c.ok("mcpServerStatus/list", map[string]any{}, &all)
	if len(all.Data) != 3 {
		t.Fatalf("servers = %+v, want three", all.Data)
	}
	checkFailedMCPServer(t, all.Data[0])
	checkConnectedMCPServer(t, all.Data[1])
	checkPlugMCPServer(t, all.Data[2])
}

func checkPlugMCPServer(t *testing.T, server mcpStatusView) {
	t.Helper()
	echo, ok := server.Tools["echo"]
	if server.Name != "plug:srv" || server.RuntimeStatus != "connected" || !ok || len(server.Tools) != 1 {
		t.Fatalf("plug server = %+v", server)
	}
	if echo.Name != "echo" {
		t.Fatalf("tool = %+v, want the server's own name echo", echo)
	}
}

func checkFailedMCPServer(t *testing.T, server mcpStatusView) {
	t.Helper()
	if server.Name != "broken" || server.RuntimeStatus != "failed" || server.ToolsError != "exit status 1" || server.AuthStatus != "unsupported" || len(server.Tools) != 0 {
		t.Fatalf("failed server = %+v", server)
	}
}

func checkConnectedMCPServer(t *testing.T, server mcpStatusView) {
	t.Helper()
	lookup, ok := server.Tools["lookup"]
	if server.Name != "docs" || server.RuntimeStatus != "connected" || server.ToolsError != "" || !ok {
		t.Fatalf("connected server = %+v", server)
	}
	if lookup.Name != "lookup" || lookup.Description != "Look up a page." || string(lookup.InputSchema) != `{"type":"object","properties":{"q":{"type":"string"}}}` {
		t.Fatalf("tool = %+v", lookup)
	}
}

func checkMCPStatusPages(t *testing.T, c *wireClient) {
	t.Helper()
	var first mcpStatusPage
	c.ok("mcpServerStatus/list", map[string]any{"limit": 1}, &first)
	if len(first.Data) != 1 || first.Data[0].Name != "broken" || first.NextCursor != "1" {
		t.Fatalf("first page = %+v", first)
	}
	checkMCPSecondPage(t, c, first.NextCursor)
	var one mcpStatusPage
	c.ok("mcpServerStatus/list", map[string]any{"serverName": "docs"}, &one)
	if len(one.Data) != 1 || one.Data[0].Name != "docs" {
		t.Fatalf("filtered page = %+v", one)
	}
}

func checkMCPSecondPage(t *testing.T, c *wireClient, cursor string) {
	t.Helper()
	var second mcpStatusPage
	c.ok("mcpServerStatus/list", map[string]any{"cursor": cursor}, &second)
	if len(second.Data) != 2 || second.Data[0].Name != "docs" || second.Data[1].Name != "plug:srv" || second.NextCursor != "" {
		t.Fatalf("second page = %+v", second)
	}
}

func TestMCPServerStatusListWithoutHook(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	m := c.call("mcpServerStatus/list", map[string]any{})
	if m.Error != nil || string(m.Result) != `{"data":[]}` {
		t.Fatalf("result = %s, error %+v; want {\"data\":[]}", m.Result, m.Error)
	}
}
