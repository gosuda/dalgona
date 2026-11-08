package agent

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/model"
)

func infoModel(t *testing.T, host *Host, threadID string) string {
	t.Helper()
	for _, info := range host.AllThreads() {
		if info.ID == threadID {
			return info.Model
		}
	}
	t.Fatalf("thread %s is not listed", threadID)
	return ""
}

func expectModel(t *testing.T, h *harness, thread *Thread, want string) {
	t.Helper()
	current, err := thread.Settings()
	if err != nil {
		t.Fatal(err)
	}
	if current.Model != want {
		t.Fatalf("effective model = %q, want %q", current.Model, want)
	}
	if got := infoModel(t, h.host, thread.ID()); got != want {
		t.Fatalf("ThreadInfo.Model = %q, want %q", got, want)
	}
}

func TestThreadInfoModelFollowsLeafBackToOverrideBranch(t *testing.T) {
	h := newHarness(t)
	thread := h.start()
	h.submit(thread, "one")
	h.waitIdle(thread)
	if err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "two", Model: "openai/gpt-y"}); err != nil {
		t.Fatal(err)
	}
	h.waitIdle(thread)
	override := turnIDs(h.events(thread.ID()))[1]
	expectModel(t, h, thread, "openai/gpt-y")
	if err := thread.Revert(override); err != nil {
		t.Fatal(err)
	}
	h.submit(thread, "three")
	h.waitIdle(thread)
	expectModel(t, h, thread, "openai/gpt-test")
	if err := thread.SetLeaf(override); err != nil {
		t.Fatal(err)
	}
	expectModel(t, h, thread, "openai/gpt-y")
	if err := h.host.Close(); err != nil {
		t.Fatal(err)
	}
	h.host = h.newHost()
	if got := infoModel(t, h.host, thread.ID()); got != "openai/gpt-y" {
		t.Fatalf("scanned ThreadInfo.Model = %q, want openai/gpt-y", got)
	}
}

type nestedOnEnd struct {
	baseExtension
	thread *Thread
	result chan error
}

func (e nestedOnEnd) TurnEnd(ctx context.Context, _, _ string) error {
	err := e.runScript(ctx)
	e.result <- err
	return err
}

func (e nestedOnEnd) runScript(ctx context.Context) error {
	source, err := e.thread.PutBlob([]byte("read a.txt"))
	if err != nil {
		return err
	}
	started := event.ScriptStarted{RunID: "run-1", SourceBlob: source, Entry: "main", Args: jsontext.Value("{}")}
	if err := e.thread.Append(ctx, started); err != nil {
		return err
	}
	nested := e.thread.RunNestedCall(ctx, event.ToolCall{ID: "run-1/1", Name: "read", Args: jsontext.Value(`{"path":"a.txt"}`)})
	completed := event.ScriptCompleted{RunID: "run-1", OK: true, Value: jsontext.Value("null")}
	return errors.Join(nested, e.thread.Append(ctx, completed))
}

func TestTurnEndNestedCallAfterRegistryFailure(t *testing.T) {
	result := make(chan error, 1)
	h := newHarness(t, func(h *harness) {
		h.cfg.Providers = func(RetryFunc) (*model.Registry, error) { return nil, errors.New("no providers") }
		h.cfg.Extension = func(th *Thread) Extension { return nestedOnEnd{thread: th, result: result} }
	})
	h.write("a.txt", "nested\n")
	thread := h.start()
	h.submit(thread, "go")
	awaitHook(t, result)
	h.waitIdle(thread)
	expectNestedReadInFailedTurn(t, h, thread.ID())
}

func awaitHook(t *testing.T, result <-chan error) {
	t.Helper()
	select {
	case err := <-result:
		if err != nil {
			t.Fatalf("nested call in turn_end: %v", err)
		}
	case <-time.After(20 * time.Second):
		t.Fatal("turn_end hook did not run")
	}
}

func expectNestedReadInFailedTurn(t *testing.T, h *harness, threadID string) {
	t.Helper()
	var ended event.TurnCompleted
	ran := false
	for _, item := range h.events(threadID) {
		switch value := item.Event.(type) {
		case event.TurnCompleted:
			ended = value
		case event.ToolCompleted:
			ran = ran || (value.CallID == "run-1/1" && value.OK)
		}
	}
	if !ran || ended.Status != statusFailed || !strings.Contains(ended.Error, "no providers") {
		t.Fatalf("nested read ran=%v, turn.completed = %+v; events: %v", ran, ended, h.types(threadID))
	}
}
