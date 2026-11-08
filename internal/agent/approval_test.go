package agent

import (
	"context"
	"fmt"
	"io"
	"math/rand/v2"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/tool"
)

type decisionsServer struct {
	server     *httptest.Server
	requests   atomic.Int64
	reversible float64
	servesTask float64
	delay      time.Duration
}

func newDecisionsServer(t *testing.T, reversible, servesTask float64) *decisionsServer {
	t.Helper()
	d := &decisionsServer{reversible: reversible, servesTask: servesTask}
	d.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		d.requests.Add(1)
		_, _ = io.Copy(io.Discard, r.Body)
		if d.delay > 0 {
			select {
			case <-time.After(d.delay):
			case <-r.Context().Done():
				return
			}
		}
		_, _ = fmt.Fprintf(w, `{"answers":{"reversible":{"type":"noul","noul":%v},"serves_task":{"type":"noul","noul":%v}}}`, d.reversible, d.servesTask)
	}))
	t.Cleanup(d.server.Close)
	return d
}

func (d *decisionsServer) decider(timeout time.Duration) Decider {
	client := decide.NewDecisions(decide.DecisionsConfig{
		BaseURL: d.server.URL, Model: "typesafe/jev-1.14", HTTPClient: d.server.Client(),
		Token: func(_ context.Context) (string, error) { return "token", nil },
	})
	adapter := NewDecider(client, "typesafe/jev-1.14", 0.9, config.ProductDalgona).(clientDecider)
	adapter.timeout = timeout
	return adapter
}

func withDecider(d *decisionsServer, timeout time.Duration) func(*harness) {
	return func(h *harness) { h.cfg.Decider = d.decider(timeout) }
}

func withApproval(mode config.Approval) func(*harness) {
	return func(h *harness) { h.cfg.Config.Approval = mode }
}

func resolutions(events []event.Seqd) map[string]event.ApprovalResolved {
	requests := make(map[string]string)
	out := make(map[string]event.ApprovalResolved)
	for _, item := range events {
		switch value := item.Event.(type) {
		case event.ApprovalRequested:
			requests[value.RequestID] = value.CallID
		case event.ApprovalResolved:
			out[requests[value.RequestID]] = value
		}
	}
	return out
}

func started(events []event.Seqd, callID string) bool {
	for _, item := range events {
		if value, ok := item.Event.(event.ToolStarted); ok && value.CallID == callID {
			return true
		}
	}
	return false
}

func runCalls(t *testing.T, h *harness, c ...call) []event.Seqd {
	t.Helper()
	h.model.push(calls(c...), text("done"))
	thread := h.start()
	h.submit(thread, "run the task")
	h.waitIdle(thread)
	return h.events(thread.ID())
}

func TestAutoReadAndPatchSkipTheDecider(t *testing.T) {
	t.Parallel()
	decisions := newDecisionsServer(t, 0.1, 0.1)
	h := newHarness(t, withDecider(decisions, time.Second))
	h.write("a.txt", "old\n")
	events := runCalls(t, h,
		call{id: "r", name: "read", args: `{"path":"a.txt"}`},
		call{id: "p", name: "patch", args: `{"changes":[{"path":"a.txt","old":"old","new":"new"}]}`},
	)
	if decisions.requests.Load() != 0 {
		t.Fatalf("decider requests = %d", decisions.requests.Load())
	}
	got := resolutions(events)
	if got["r"].By != "mode:auto" || got["p"].By != "mode:auto" || !started(events, "p") {
		t.Fatalf("resolutions = %+v", got)
	}
}

func TestDeciderScoresGateExec(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name               string
		reversible, serves float64
		allow              bool
		reason             string
	}{
		{name: "allowed", reversible: 0.93, serves: 0.95, allow: true},
		{name: "declined", reversible: 0.45, serves: 0.09, reason: "declined by decider typesafe/jev-1.14: reversible=0.45 serves_task=0.09 (threshold 0.9)"},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			decisions := newDecisionsServer(t, test.reversible, test.serves)
			h := newHarness(t, withDecider(decisions, time.Second))
			events := runCalls(t, h, call{id: "x", name: "exec", args: `{"command":"go test ./..."}`})
			resolved := resolutions(events)["x"]
			if decisions.requests.Load() != 1 || started(events, "x") != test.allow {
				t.Fatalf("requests = %d, started = %v, resolved = %+v", decisions.requests.Load(), started(events, "x"), resolved)
			}
			if !test.allow && (resolved.Reason != test.reason || completedOutputs(events)["x"].Output != test.reason) {
				t.Fatalf("reason = %q, output = %q", resolved.Reason, completedOutputs(events)["x"].Output)
			}
		})
	}
}

func TestStaticRuleDeclinesWithoutDecider(t *testing.T) {
	t.Parallel()
	decisions := newDecisionsServer(t, 1, 1)
	h := newHarness(t, withDecider(decisions, time.Second))
	events := runCalls(t, h, call{id: "x", name: "exec", args: `{"command":"git push --force"}`})
	resolved := resolutions(events)["x"]
	if decisions.requests.Load() != 0 || resolved.By != "rule:git-destructive" || started(events, "x") {
		t.Fatalf("requests = %d, resolved = %+v", decisions.requests.Load(), resolved)
	}
	if !strings.HasPrefix(resolved.Reason, "declined by rule git-destructive: ") {
		t.Fatalf("reason = %q", resolved.Reason)
	}
}

func TestDeciderTimeoutDeclines(t *testing.T) {
	t.Parallel()
	decisions := newDecisionsServer(t, 1, 1)
	decisions.delay = time.Second
	h := newHarness(t, withDecider(decisions, 50*time.Millisecond))
	events := runCalls(t, h, call{id: "x", name: "exec", args: `{"command":"go version"}`})
	resolved := resolutions(events)["x"]
	if started(events, "x") || !strings.HasPrefix(resolved.Reason, "declined by decider typesafe/jev-1.14: ") {
		t.Fatalf("resolved = %+v", resolved)
	}
}

func TestYoloSkipsDeciderAndSandbox(t *testing.T) {
	t.Parallel()
	decisions := newDecisionsServer(t, 0, 0)
	h := newHarness(t, withDecider(decisions, time.Second), withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.Config.Sandbox = config.SandboxWorkspaceWrite
	})
	events := runCalls(t, h, call{id: "x", name: "exec", args: `{"command":"echo yolo > ../yolo-outside"}`})
	resolved := resolutions(events)["x"]
	if decisions.requests.Load() != 0 || resolved.By != "mode:yolo" || !completedOutputs(events)["x"].OK {
		t.Fatalf("requests = %d, resolved = %+v, output = %+v", decisions.requests.Load(), resolved, completedOutputs(events)["x"])
	}
}

func TestAskSessionGrantCoversMatchingCommands(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalAsk))
	h.model.push(
		calls(call{id: "a", name: "exec", args: `{"command":"go test ./..."}`}),
		calls(call{id: "b", name: "exec", args: `{"command":"go test -run X"}`}),
		calls(call{id: "c", name: "exec", args: `{"command":"go build"}`}),
		text("done"),
	)
	thread := h.start()
	client := answerApprovals(t, thread, func(prompt ApprovalPrompt) Reply {
		if prompt.CallID == "c" {
			return ReplyDecline
		}
		return ReplyAcceptSession
	})
	h.submit(thread, "test and build")
	h.waitIdle(thread)
	prompts := client.stop()
	got := resolutions(h.events(thread.ID()))
	if got["b"].By != "grant:go test" || got["c"].By != "user" || got["c"].Decision != "decline" {
		t.Fatalf("resolutions = %+v", got)
	}
	if len(prompts) != 2 || prompts[0].CallID != "a" || prompts[1].CallID != "c" {
		t.Fatalf("prompts = %+v", prompts)
	}
}

type approvalClient struct {
	thread  *Thread
	updates <-chan Update
	done    chan struct{}
	prompts []ApprovalPrompt
}

func answerApprovals(t *testing.T, thread *Thread, decide func(ApprovalPrompt) Reply) *approvalClient {
	t.Helper()
	client := &approvalClient{thread: thread, updates: thread.Subscribe(), done: make(chan struct{})}
	go func() {
		defer close(client.done)
		for update := range client.updates {
			if update.Kind != UpdateApproval {
				continue
			}
			client.prompts = append(client.prompts, update.Approval)
			if err := thread.Answer(update.Approval.RequestID, decide(update.Approval)); err != nil {
				t.Error(err)
			}
		}
	}()
	return client
}

func (c *approvalClient) stop() []ApprovalPrompt {
	c.thread.Unsubscribe(c.updates)
	<-c.done
	return c.prompts
}

func TestAskWithoutClientDeclines(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalAsk))
	events := runCalls(t, h, call{id: "x", name: "exec", args: `{"command":"go version"}`})
	if resolved := resolutions(events)["x"]; resolved.Reason != declineNoClient || started(events, "x") {
		t.Fatalf("resolved = %+v", resolved)
	}
}

type countingDecider struct{ calls atomic.Int64 }

func (c *countingDecider) Decide(context.Context, Action) (Decision, error) {
	c.calls.Add(1)
	return Decision{Allow: true, By: "decider:count"}, nil
}

func TestSubstitutionNeverReachesDecider(t *testing.T) {
	t.Parallel()
	counter := &countingDecider{}
	h := newHarness(t, func(h *harness) { h.cfg.Decider = counter })
	thread := h.start()
	w := &worker{t: thread, turnID: "t"}
	random := rand.New(rand.NewPCG(5, 8))
	words := []string{"ls", "echo hi", "go test ./...", "cat a.txt", "printf x"}
	forms := []func(string) string{
		func(s string) string { return "echo $(" + s + ")" },
		func(s string) string { return "echo `" + s + "`" },
		func(s string) string { return "diff <(" + s + ") b" },
		func(s string) string { return s + " > >(" + s + ")" },
		func(s string) string { return s + " && x=$(" + s + ")" },
	}
	for range 300 {
		inner := words[random.IntN(len(words))]
		command := forms[random.IntN(len(forms))](inner)
		if random.IntN(2) == 0 {
			command = words[random.IntN(len(words))] + "; " + command
		}
		v := w.decide(t.Context(), gateCall{call: event.ToolCall{ID: "c", Name: "exec"}, class: tool.ClassExec, command: command}, "r")
		if v.allow || !strings.HasPrefix(v.by, "rule:") {
			t.Fatalf("command %q reached %+v", command, v)
		}
	}
	if counter.calls.Load() != 0 {
		t.Fatalf("decider calls = %d", counter.calls.Load())
	}
}
