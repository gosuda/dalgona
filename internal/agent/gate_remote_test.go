package agent

import (
	"context"
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/tool"
)

type searchTool struct{ runs *int }

func (searchTool) Spec() tool.Spec {
	return tool.Spec{
		Name:        "srv__search",
		Description: "remote search",
		Schema:      jsontext.Value(`{"type":"object","properties":{"query":{"type":"string"},"path":{"type":"string"}}}`),
	}
}

func (searchTool) Class() tool.Class { return tool.ClassRemote }

func (s searchTool) Run(context.Context, tool.Call, tool.Env) (tool.Result, error) {
	*s.runs++
	return tool.Result{Output: []byte("found"), OK: true}, nil
}

func remoteHarness(t *testing.T, counter *countingDecider, runs *int) *harness {
	t.Helper()
	return newHarness(t, func(h *harness) {
		h.cfg.Decider = counter
		h.cfg.Tools = func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{searchTool{runs: runs}}, nil
		}
	})
}

func TestRemoteJSONArgsReachDecider(t *testing.T) {
	t.Parallel()
	counter := &countingDecider{}
	var runs int
	h := remoteHarness(t, counter, &runs)
	events := runCalls(t, h, call{id: "x", name: "srv__search", args: `{"query":"x"}`})
	if counter.calls.Load() != 1 {
		t.Fatalf("decider calls = %d, want 1 (remote call: %+v)", counter.calls.Load(), resolutions(events)["x"])
	}
	if !started(events, "x") {
		t.Fatalf("remote call did not start: %+v", resolutions(events)["x"])
	}
}

func TestRemoteSecretValueStillDeclined(t *testing.T) {
	t.Parallel()
	counter := &countingDecider{}
	var runs int
	h := remoteHarness(t, counter, &runs)
	events := runCalls(t, h, call{id: "x", name: "srv__search", args: `{"path":".env"}`})
	resolved := resolutions(events)["x"]
	if counter.calls.Load() != 0 || resolved.By != "rule:credentials" || started(events, "x") {
		t.Fatalf("calls = %d, resolved = %+v", counter.calls.Load(), resolved)
	}
}
