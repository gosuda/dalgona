package app

import (
	"fmt"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/decide"
)

func TestDeciderIsAgentOwned(t *testing.T) {
	j := &judges{
		roles: &roleResolver{},
		spec:  decide.Spec{Adapter: decide.AdapterLLM, Model: "smol"},
	}
	if name := fmt.Sprintf("%T", j.decider()); !strings.HasPrefix(name, "agent.") {
		t.Fatalf("decider() = %s, want an agent-owned Decider without an app wrapper", name)
	}
}

func TestDecideWithoutHostReportsNotReady(t *testing.T) {
	j := &judges{
		roles: &roleResolver{},
		spec:  decide.Spec{Adapter: decide.AdapterLLM, Model: "smol"},
	}
	_, err := j.decider().Decide(t.Context(), agent.Action{Thread: "thread-x"})
	if err == nil || !strings.Contains(err.Error(), "host is not ready") {
		t.Fatalf("Decide without a host = %v, want host-not-ready", err)
	}
}
