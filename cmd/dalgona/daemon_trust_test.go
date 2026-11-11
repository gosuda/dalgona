package main

import (
	"testing"

	"github.com/gosuda/dalgona/internal/app/battery"
	"github.com/gosuda/dalgona/internal/discovery"
)

func TestWorkspaceTrustDecisionWithoutThreadCancels(t *testing.T) {
	t.Parallel()
	servers := []discovery.MCPServer{{Name: "wsfix", Command: "/tmp/srv", Workspace: true}}
	if got := workspaceTrustDecision(t.Context(), "/work/repo", servers); got != battery.TrustCancel {
		t.Fatalf("workspaceTrustDecision without thread = %v, want cancel so the next turn asks again", got)
	}
}
