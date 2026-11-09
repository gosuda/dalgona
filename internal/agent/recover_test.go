package agent

import (
	"context"
	"encoding/json/jsontext"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

const crashSession = "crashed"

func (h *harness) seed(events ...event.Event) {
	h.t.Helper()
	created := event.SessionCreated{
		ID: crashSession, Cwd: h.cwd, Model: h.cfg.Config.Model, Approval: string(h.cfg.Config.Approval),
		Sandbox: "off", Runner: "local",
	}
	raws := make([][]byte, 0, len(events)+1)
	for _, item := range append([]event.Event{created}, events...) {
		raw, err := event.Encode(item)
		if err != nil {
			h.t.Fatal(err)
		}
		raws = append(raws, raw)
	}
	if _, err := h.journal.Append(h.t.Context(), crashSession, raws...); err != nil {
		h.t.Fatal(err)
	}
}

func (h *harness) recoverSession() []event.Seqd {
	h.t.Helper()
	host := h.newHost()
	if err := host.Recover(h.t.Context()); err != nil {
		h.t.Fatal(err)
	}
	thread, err := host.Resume(h.t.Context(), crashSession)
	if err != nil {
		h.t.Fatal(err)
	}
	h.waitIdle(thread)
	events := h.events(crashSession)
	if turn := lastTurn(events); turn.TurnID != "t" {
		h.t.Fatalf("recovered turn did not complete: %v", h.types(crashSession))
	}
	return events
}

func openTurn(prompt string) []event.Event {
	return []event.Event{
		event.TurnStarted{TurnID: "t"},
		event.UserMessage{ItemID: "u", TurnID: "t", Text: prompt, Kind: "prompt"},
	}
}

func assistantCall(id, name, args string) event.AssistantMessage {
	return event.AssistantMessage{
		ItemID: "a-" + id, TurnID: "t", StopReason: "tool_calls",
		ToolCalls: []event.ToolCall{{ID: id, Name: name, Args: jsontext.Value(args)}},
	}
}

func approved(id, class string) []event.Event {
	return []event.Event{
		event.ApprovalRequested{RequestID: "r-" + id, CallID: id, Summary: "run", Class: class},
		event.ApprovalResolved{RequestID: "r-" + id, Decision: "accept", By: "mode:auto", Reason: "allowed"},
	}
}

func startedCall(id, name, class, args string, files ...event.FileChange) event.ToolStarted {
	return event.ToolStarted{CallID: id, TurnID: "t", Tool: name, Class: class, Args: jsontext.Value(args), Files: files}
}

func prefix(parts ...[]event.Event) []event.Event {
	var out []event.Event
	for _, part := range parts {
		out = append(out, part...)
	}
	return out
}

func TestRecoverOpenTurnCallsModelAgain(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.seed(openTurn("resume me")...)
	events := h.recoverSession()
	if turn := lastTurn(events); turn.Status != statusCompleted || h.model.count() != 1 {
		t.Fatalf("turn = %+v, requests = %d", turn, h.model.count())
	}
	if texts := userTexts(h.model.request(0)); len(texts) != 1 || texts[0] != "resume me" {
		t.Fatalf("texts = %q", texts)
	}
}

func TestRecoverSkipsUnstartedCall(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.seed(prefix(openTurn("go"), []event.Event{assistantCall("c1", "read", `{"path":"a.txt"}`)})...)
	events := h.recoverSession()
	got := completedOutputs(events)["c1"]
	if got.Recovered != "skipped" || got.Output != "skipped: "+skipRestarted {
		t.Fatalf("completion = %+v", got)
	}
	if outputs := callOutputs(h.model.request(0)); outputs["c1"] != got.Output {
		t.Fatalf("next request outputs = %v", outputs)
	}
}

func TestRecoverRerunsReadInFlight(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.write("a.txt", "alpha\n")
	h.seed(prefix(openTurn("go"), []event.Event{assistantCall("c1", "read", `{"path":"a.txt"}`)}, approved("c1", "read"),
		[]event.Event{startedCall("c1", "read", "read", `{"path":"a.txt"}`)})...)
	got := completedOutputs(h.recoverSession())["c1"]
	if !got.OK || got.Recovered != "replayed" || !strings.Contains(got.Output, "alpha") {
		t.Fatalf("completion = %+v", got)
	}
}

func (h *harness) fileChange(path, pre, post string) event.FileChange {
	h.t.Helper()
	change := event.FileChange{Path: path, PreSHA: snapshot.Digest([]byte(pre)), PostSHA: snapshot.Digest([]byte(post))}
	sha, err := h.journal.PutBlob([]byte(pre))
	if err != nil {
		h.t.Fatal(err)
	}
	change.PreBlob = &event.BlobRef{Blob: sha, Bytes: int64(len(pre))}
	return change
}

func (h *harness) read(rel string) string {
	h.t.Helper()
	data, err := os.ReadFile(filepath.Join(h.cwd, rel))
	if err != nil {
		h.t.Fatal(err)
	}
	return string(data)
}

func TestRecoverEditInFlight(t *testing.T) {
	t.Parallel()
	args := `{"changes":[{"path":"a.txt","old":"old","new":"new"}]}`
	cases := []struct {
		name      string
		current   string
		recovered string
		ok        bool
		final     string
	}{
		{name: "applied", current: "new\n", recovered: "replayed", ok: true, final: "new\n"},
		{name: "untouched", current: "old\n", recovered: "replayed", ok: true, final: "new\n"},
		{name: "foreign", current: "other\n", recovered: "interrupted", final: "other\n"},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			h := newHarness(t)
			h.write("a.txt", test.current)
			change := h.fileChange("a.txt", "old\n", "new\n")
			h.seed(prefix(openTurn("edit"), []event.Event{assistantCall("c1", "patch", args)}, approved("c1", "edit"),
				[]event.Event{startedCall("c1", "patch", "edit", args, change)})...)
			got := completedOutputs(h.recoverSession())["c1"]
			if got.OK != test.ok || got.Recovered != test.recovered || h.read("a.txt") != test.final {
				t.Fatalf("completion = %+v, file = %q", got, h.read("a.txt"))
			}
		})
	}
}

func TestRecoverMixedEditRestoresPostFiles(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.write("a.txt", "a-new\n")
	h.write("b.txt", "b-foreign\n")
	args := `{"changes":[{"path":"a.txt","old":"a-old","new":"a-new"},{"path":"b.txt","old":"b-old","new":"b-new"}]}`
	files := []event.FileChange{h.fileChange("a.txt", "a-old\n", "a-new\n"), h.fileChange("b.txt", "b-old\n", "b-new\n")}
	h.seed(prefix(openTurn("edit"), []event.Event{assistantCall("c1", "patch", args)}, approved("c1", "edit"),
		[]event.Event{startedCall("c1", "patch", "edit", args, files...)})...)
	got := completedOutputs(h.recoverSession())["c1"]
	if got.Output != "interrupted: files changed during recovery: a.txt, b.txt" || got.Recovered != "interrupted" {
		t.Fatalf("completion = %+v", got)
	}
	if h.read("a.txt") != "a-old\n" || h.read("b.txt") != "b-foreign\n" {
		t.Fatalf("files a=%q b=%q", h.read("a.txt"), h.read("b.txt"))
	}
}

func (h *harness) startExitedProcess(id, command string) {
	h.t.Helper()
	h.startProcessUntil(id, command, func(out runner.Output) bool { return out.Exit != nil })
}

func (h *harness) startRunningProcess(id, command string) {
	h.t.Helper()
	h.startProcessUntil(id, command, func(out runner.Output) bool { return out.Alive || out.Exit != nil })
}

func (h *harness) startProcessUntil(id, command string, ready func(runner.Output) bool) {
	h.t.Helper()
	local, err := runner.NewLocal(filepath.Join(h.home, "sessions", crashSession, "runs"))
	if err != nil {
		h.t.Fatal(err)
	}
	defer func() { _ = local.Close() }()
	spec := runner.ProcessSpec{ID: id, Argv: []string{"sh", "-c", command}, Dir: h.cwd, Sandbox: runner.Sandbox{Mode: runner.SandboxOff}}
	if err := local.Start(h.t.Context(), spec); err != nil {
		h.t.Fatal(err)
	}
	for deadline := time.Now().Add(10 * time.Second); time.Now().Before(deadline); time.Sleep(10 * time.Millisecond) {
		if out, err := local.Read(h.t.Context(), id, 0); err == nil && ready(out) {
			return
		}
	}
	h.t.Fatal("process did not reach the expected state")
}

func execPrefix(id, command string) []event.Event {
	args := `{"command":` + string(jsonString(command)) + `}`
	return prefix(openTurn("run"), []event.Event{assistantCall(id, "exec", args)}, approved(id, "exec"),
		[]event.Event{startedCall(id, "exec", "exec", args)})
}

func TestRecoverExecThatExited(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.startExitedProcess("c1", "echo finished")
	h.seed(execPrefix("c1", "echo finished")...)
	got := completedOutputs(h.recoverSession())["c1"]
	if !got.OK || !strings.Contains(got.Output, "finished") || got.ExitCode == nil || *got.ExitCode != 0 {
		t.Fatalf("completion = %+v", got)
	}
}

func TestRecoverExecStillRunning(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.startRunningProcess("c1", "sleep 1; echo late")
	h.seed(execPrefix("c1", "sleep 1; echo late")...)
	events := h.recoverSession()
	got := completedOutputs(events)["c1"]
	if !got.OK || !strings.Contains(got.Output, "late") {
		t.Fatalf("completion = %+v, turn = %+v", got, lastTurn(events))
	}
}

func TestRecoverExecNeverSpawnedIsNotRerun(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.seed(execPrefix("c1", "touch ran-marker")...)
	got := completedOutputs(h.recoverSession())["c1"]
	if got.OK || got.Recovered != "interrupted" || got.Output != execInterrupted {
		t.Fatalf("completion = %+v", got)
	}
	if _, err := os.Stat(filepath.Join(h.cwd, "ran-marker")); !os.IsNotExist(err) {
		t.Fatalf("recovery ran the command again: %v", err)
	}
}

func TestRecoverRedecidesOpenApproval(t *testing.T) {
	t.Parallel()
	counter := &countingDecider{}
	h := newHarness(t, func(h *harness) { h.cfg.Decider = counter })
	h.seed(prefix(openTurn("run"), []event.Event{
		assistantCall("c1", "exec", `{"command":"echo decided"}`),
		event.ApprovalRequested{RequestID: "r-c1", CallID: "c1", Summary: "echo decided", Class: "exec"},
	})...)
	events := h.recoverSession()
	got := completedOutputs(events)["c1"]
	if counter.calls.Load() != 1 || !got.OK || !strings.Contains(got.Output, "decided") {
		t.Fatalf("decider calls = %d, completion = %+v", counter.calls.Load(), got)
	}
}

func TestRecoverAskWithoutClientDeclines(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalAsk))
	h.seed(prefix(openTurn("run"), []event.Event{
		assistantCall("c1", "exec", `{"command":"echo nope"}`),
		event.ApprovalRequested{RequestID: "r-c1", CallID: "c1", Summary: "echo nope", Class: "exec"},
	})...)
	got := completedOutputs(h.recoverSession())["c1"]
	if got.Output != declineNoClient {
		t.Fatalf("completion = %+v", got)
	}
}

func TestRecoverAcceptedUnstartedCallKeepsGrant(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalAsk))
	h.seed(prefix(openTurn("run"), []event.Event{
		assistantCall("c1", "exec", `{"command":"go test ./..."}`),
		event.ApprovalRequested{RequestID: "r-c1", CallID: "c1", Summary: "go test ./...", Class: "exec"},
		event.ApprovalResolved{RequestID: "r-c1", Decision: "accept_session", By: "user", Reason: "allowed for this session"},
	})...)
	events := h.recoverSession()
	if got := completedOutputs(events)["c1"]; got.Recovered != "skipped" || started(events, "c1") {
		t.Fatalf("completion = %+v", got)
	}
	loaded, err := h.host.load(crashSession)
	if err != nil {
		t.Fatal(err)
	}
	if loaded.state.Grants["go test"] != "exec" {
		t.Fatalf("grants = %#v", loaded.state.Grants)
	}
}

type remoteTool struct{ runs *int }

func (remoteTool) Spec() tool.Spec {
	return tool.Spec{Name: "srv__deploy", Description: "remote", Schema: jsontext.Value(`{"type":"object"}`)}
}
func (remoteTool) Class() tool.Class { return tool.ClassRemote }
func (r remoteTool) Run(context.Context, tool.Call, tool.Env) (tool.Result, error) {
	*r.runs++
	return tool.Result{Output: []byte("deployed"), OK: true}, nil
}

func TestRecoverRemoteCallIsInterruptedNotRerun(t *testing.T) {
	t.Parallel()
	runs := 0
	h := newHarness(t, func(h *harness) {
		h.cfg.Tools = func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{remoteTool{runs: &runs}}, nil
		}
	})
	h.seed(prefix(openTurn("deploy"), []event.Event{assistantCall("c1", "srv__deploy", `{}`)}, approved("c1", "remote"),
		[]event.Event{startedCall("c1", "srv__deploy", "remote", `{}`)})...)
	got := completedOutputs(h.recoverSession())["c1"]
	if runs != 0 || got.OK || got.Recovered != "interrupted" || got.Output != remoteInterrupt {
		t.Fatalf("runs = %d, completion = %+v", runs, got)
	}
}
