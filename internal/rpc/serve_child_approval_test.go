package rpc

import (
	"context"
	json "encoding/json/v2"
	"log/slog"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/journal"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/exec"
)

func newDalgonaRig(t *testing.T) *rig {
	t.Helper()
	r := &rig{t: t, home: t.TempDir(), cwd: t.TempDir(), llm: newFakeLLM(t)}
	t.Setenv("DAL_HOME", r.home)
	if resolved, err := filepath.EvalSymlinks(r.cwd); err == nil {
		r.cwd = resolved
	}
	store, err := journal.Open(r.home)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	credentials, err := auth.Load(r.home)
	if err != nil {
		t.Fatal(err)
	}
	host, err := agent.NewHost(agent.HostConfig{
		Journal: store, Auth: credentials, ProviderIDs: []string{"openai"},
		Config: config.Config{
			Home: r.home, Cwd: r.cwd, Model: "openai/gpt-test", Approval: config.ApprovalAsk, Sandbox: config.SandboxOff,
			Product: config.ProductDalgona, Reasoning: config.ReasoningDefault,
		},
		Providers: r.providers,
		Tools:     func(context.Context, string, string) ([]tool.Tool, error) { return []tool.Tool{exec.New()}, nil },
		Runner: func(_ context.Context, id, _ string) (runner.Runner, error) {
			return runner.NewLocal(filepath.Join(r.home, "sessions", id, "runs"))
		},
		Logger: slog.New(slog.DiscardHandler),
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = host.Close() })
	r.host = host
	return r
}

func childApprovalRequest(t *testing.T, r *rig, c *wireClient, execArgs string) (message, string) {
	t.Helper()
	r.llm.push(
		llmReply{body: sseCall("call-task", "task", `{"prompt":"run it"}`)},
		llmReply{body: sseCall("call-child", "exec", execArgs)},
		llmReply{body: sseCall("call-yield", "yield", `{"data":"ok"}`)},
		llmReply{body: sseText("parent done")},
	)
	parentID := startThread(t, r, c, nil)
	startTurn(t, c, parentID, "delegate")
	var request message
	select {
	case request = <-c.requests:
	case <-time.After(waitLimit):
		t.Fatal("no child approval request on the parent connection")
	}
	childID := fieldOf(t, request.Params, "threadId")
	expect(t, request.Method == "item/commandExecution/requestApproval" && fieldOf(t, request.Params, "itemId") == "call-child",
		"child approval request", string(request.Params))
	expect(t, childID != parentID && childID != "", "child thread id", childID)
	return request, childID
}

func TestServeChildApprovalResolvedReachesParentConnection(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	request, childID := childApprovalRequest(t, r, c, `{"command":"echo child","justification":"prints a marker"}`)
	expect(t, fieldOf(t, request.Params, "reason") == "prints a marker", "approval reason from justification", string(request.Params))
	var requestID string
	if err := json.Unmarshal(request.ID, &requestID); err != nil {
		t.Fatal(err)
	}
	c.respond(request.ID, `{"decision":"accept"}`)
	resolved := c.await("serverRequest/resolved", func(m message) bool {
		return fieldOf(t, m.Params, "requestId") == requestID
	})
	expect(t, fieldOf(t, resolved.Params, "threadId") == childID, "serverRequest/resolved thread", string(resolved.Params))
	log := readTurn(t, c.collect("turn/completed"))
	expect(t, log.status == statusCompleted, "parent turn status", log.status)
	got := childResolution(t, r, childID)
	expect(t, got.RequestID == requestID && got.Decision == "accept", "child resolution", got)
}

func TestServeChildApprovalDeclinedWhenParentConnectionLeaves(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	request, childID := childApprovalRequest(t, r, c, `{"command":"echo child"}`)
	expect(t, !strings.Contains(string(request.Params), `"reason"`), "approval reason without justification", string(request.Params))
	var requestID string
	if err := json.Unmarshal(request.ID, &requestID); err != nil {
		t.Fatal(err)
	}
	_ = c.conn.Close()
	got := childResolution(t, r, childID)
	expect(t, got.RequestID == requestID && got.Decision == "decline", "child resolution after the parent connection left", got)
}

func TestServeChildApprovalDeclinedWithoutParentConnection(t *testing.T) {
	r := newDalgonaRig(t)
	c := r.connect()
	gate := make(chan struct{})
	r.llm.push(
		llmReply{body: sseCall("call-task", "task", `{"prompt":"run it"}`)},
		llmReply{body: sseCall("call-child", "exec", `{"command":"echo child"}`), gate: gate},
		llmReply{body: sseCall("call-yield", "yield", `{"data":"ok"}`)},
		llmReply{body: sseText("parent done")},
	)
	parentID := startThread(t, r, c, nil)
	startTurn(t, c, parentID, "delegate")
	childID := eventually(t, "child thread", func() (string, bool) { return childOfThread(r, parentID) })
	_ = c.conn.Close()
	parent, err := r.host.Resume(t.Context(), parentID)
	if err != nil {
		t.Fatal(err)
	}
	eventually(t, "parent without subscribers", func() (int, bool) {
		status, err := parent.Status()
		return status.Subscribers, err == nil && status.Subscribers == 0
	})
	close(gate)
	got := childResolution(t, r, childID)
	expect(t, got.Decision == "decline" && got.Reason == "declined: no client could approve this call", "headless child resolution", got)
}

func childOfThread(r *rig, parentID string) (string, bool) {
	for _, info := range r.host.AllThreads() {
		if info.Parent == parentID {
			return info.ID, true
		}
	}
	return "", false
}

func eventually[T any](t *testing.T, what string, probe func() (T, bool)) T {
	t.Helper()
	deadline := time.Now().Add(waitLimit)
	for {
		value, ok := probe()
		if ok {
			return value
		}
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s; last %v", what, value)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func childResolution(t *testing.T, r *rig, childID string) event.ApprovalResolved {
	t.Helper()
	child, err := r.host.Resume(t.Context(), childID)
	if err != nil {
		t.Fatal(err)
	}
	return eventually(t, "child approval resolution", func() (event.ApprovalResolved, bool) {
		read, err := child.ReadAfter(0)
		if err != nil {
			t.Fatal(err)
		}
		return resolvedCall(read.Events, "call-child")
	})
}

func resolvedCall(events []event.Seqd, callID string) (event.ApprovalResolved, bool) {
	var requestID string
	for _, e := range events {
		switch value := e.Event.(type) {
		case event.ApprovalRequested:
			if value.CallID == callID {
				requestID = value.RequestID
			}
		case event.ApprovalResolved:
			if requestID != "" && value.RequestID == requestID {
				return value, true
			}
		}
	}
	return event.ApprovalResolved{}, false
}
