package agent

import (
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
)

func startSpecThread(t *testing.T, h *harness, spec ThreadSpec) *Thread {
	t.Helper()
	thread, err := h.host.Start(t.Context(), spec)
	if err != nil {
		t.Fatal(err)
	}
	return thread
}

func runSpecCalls(t *testing.T, h *harness, spec ThreadSpec, prompt string, c ...call) (*Thread, []event.Seqd) {
	t.Helper()
	h.model.push(calls(c...), text("done"))
	thread := startSpecThread(t, h, spec)
	h.submit(thread, prompt)
	h.waitIdle(thread)
	return thread, h.events(thread.ID())
}

func requireRefused(t *testing.T, events []event.Seqd, id string) {
	t.Helper()
	got := completedOutputs(events)[id]
	if got.OK || !strings.Contains(got.Output, "read-only") {
		t.Fatalf("call %s output = %+v", id, got)
	}
	if started(events, id) {
		t.Fatalf("call %s started", id)
	}
	if resolutions(events)[id].By != "read-only" {
		t.Fatalf("call %s resolution = %+v", id, resolutions(events)[id])
	}
}

func TestReadOnlyThreadRefusesWriteAndExec(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.write("a.txt", threeLines)
	_, events := runSpecCalls(t, h, ThreadSpec{ReadOnly: true}, "run the task",
		call{id: "r", name: "read", args: `{"path":"a.txt"}`},
		call{id: "p", name: "patch", args: `{"changes":[{"path":"a.txt","old":"one","new":"ONE"}]}`},
		call{id: "x", name: "exec", args: `{"command":"touch ro-exec-proof.txt"}`},
	)
	if !completedOutputs(events)["r"].OK {
		t.Fatalf("read output = %+v", completedOutputs(events)["r"])
	}
	requireRefused(t, events, "p")
	requireRefused(t, events, "x")
	if content := h.read("a.txt"); content != threeLines {
		t.Fatalf("a.txt = %q, want unchanged", content)
	}
	if _, err := os.Stat(filepath.Join(h.cwd, "ro-exec-proof.txt")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("ro-exec-proof.txt stat error = %v", err)
	}
}

func TestThreadCarriesConfiguredProfile(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := startSpecThread(t, h, ThreadSpec{Profile: "hashline-light"})
	defer closeThread(t, thread)
	var created event.SessionCreated
	for _, item := range h.events(thread.ID()) {
		if value, ok := item.Event.(event.SessionCreated); ok {
			created = value
		}
	}
	if created.Profile != "hashline-light" {
		t.Fatalf("session profile = %q, want hashline-light", created.Profile)
	}
	set, err := thread.toolsFor(t.Context(), h.cfg.Config.Model)
	if err != nil {
		t.Fatal(err)
	}
	item, ok := set.lookup("patch")
	if !ok {
		t.Fatal("no patch tool in thread toolset")
	}
	profile, named := profileOf(item)
	if !named || profile.Style != "hashline-light" {
		t.Fatalf("patch profile = %+v named %v", profile, named)
	}
}

func TestReadOnlyRecoveryStaysReadOnly(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.write("a.txt", threeLines)
	thread := startSpecThread(t, h, ThreadSpec{ReadOnly: true, Profile: "hashline-light"})
	id := thread.ID()
	closeThread(t, thread)
	resumed := resumeOrFail(t, h.host, id)
	defer closeThread(t, resumed)
	if !resumed.created.ReadOnly || resumed.created.Profile != "hashline-light" {
		t.Fatalf("recovered created = %+v", resumed.created)
	}
	h.model.push(
		calls(call{id: "x", name: "exec", args: `{"command":"touch ro-recovery-proof.txt"}`}),
		text("done"),
	)
	h.submit(resumed, "edit the file")
	h.waitIdle(resumed)
	events := h.events(id)
	requireRefused(t, events, "x")
	if _, err := os.Stat(filepath.Join(h.cwd, "ro-recovery-proof.txt")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("ro-recovery-proof.txt stat error = %v", err)
	}
}

func TestReadOnlyInBandProfiledCallRefused(t *testing.T) {
	t.Parallel()
	h := newProfileHarness(t, "hashline-light")
	h.write("a.txt", threeLines)
	thread := startSpecThread(t, h, ThreadSpec{ReadOnly: true, Profile: "hashline-light"})
	defer closeThread(t, thread)
	startInBandTurn(t, thread)
	set, err := thread.toolsFor(t.Context(), "openai/gpt-test")
	if err != nil {
		t.Fatal(err)
	}
	calls := []struct{ id, args string }{
		{"c1", `{"input":"not a patch"}`},
		{"c2", createCall},
	}
	for _, item := range calls {
		w := &worker{t: thread, turnID: "turn-test", settings: Settings{Approval: config.ApprovalAsk}, tools: set, inBand: newInBandCalls()}
		completed, err := w.inBandEvents(t.Context(), model.ToolCall{ID: item.id, Name: "patch", Args: jsontext.Value(item.args)})
		if err != nil {
			t.Fatal(err)
		}
		if completed.OK || !strings.Contains(completed.Output, "read-only") {
			t.Fatalf("in-band patch %s output = %+v", item.id, completed)
		}
		if started(h.events(thread.ID()), item.id) {
			t.Fatalf("in-band patch %s started", item.id)
		}
	}
	if content := h.read("a.txt"); content != threeLines {
		t.Fatalf("a.txt = %q, want unchanged", content)
	}
	if _, err := os.Stat(filepath.Join(h.cwd, "n.txt")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("n.txt stat error = %v", err)
	}
}
