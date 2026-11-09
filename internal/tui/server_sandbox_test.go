package tui

import "testing"

func TestServerSandboxMapping(t *testing.T) {
	t.Parallel()
	if got := serverSandbox("dangerFullAccess", "workspace-write"); got != "off" {
		t.Fatalf("dangerFullAccess maps to %q, want off", got)
	}
	if got := serverSandbox("workspaceWrite", "off"); got != "workspace-write" {
		t.Fatalf("workspaceWrite maps to %q, want workspace-write", got)
	}
	if got := serverSandbox("unknown", "workspace-write"); got != "workspace-write" {
		t.Fatalf("unknown maps to %q, want fallback", got)
	}
}
