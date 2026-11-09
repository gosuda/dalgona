package agent

import (
	"context"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

type resumingExtension struct {
	baseExtension
	t       *Thread
	mu      *sync.Mutex
	resumed *[]string
}

func (r resumingExtension) ResumeScript(ctx context.Context, runID string) error {
	r.mu.Lock()
	*r.resumed = append(*r.resumed, runID)
	r.mu.Unlock()
	return r.t.Append(ctx, event.ScriptCompleted{RunID: runID, OK: true, Value: []byte("1")})
}

func withResumer(mu *sync.Mutex, resumed *[]string) func(*harness) {
	return func(h *harness) {
		h.cfg.Extension = func(t *Thread) Extension { return resumingExtension{t: t, mu: mu, resumed: resumed} }
	}
}

func workflowTurn() []event.Event {
	return []event.Event{
		event.TurnStarted{TurnID: "t"},
		event.UserMessage{ItemID: "u", TurnID: "t", Text: scriptPrompt + "flow.star", Kind: "prompt", Verbatim: true},
	}
}

func TestRecoverWorkflowTurnReplaysTheScriptWithoutTheModel(t *testing.T) {
	t.Parallel()
	var mu sync.Mutex
	var resumed []string
	h := newHarness(t, withResumer(&mu, &resumed))
	sha, err := h.journal.PutBlob([]byte("1"))
	if err != nil {
		t.Fatal(err)
	}
	started := event.ScriptStarted{RunID: crashSession, Entry: "flow.star", SourceBlob: event.BlobRef{Blob: sha, Bytes: 1}, Args: []byte("null")}
	h.seed(prefix(workflowTurn(), []event.Event{started, startedCall(crashSession+"/1", "exec", "exec", `{"command":"true"}`)})...)
	events := h.recoverSession()
	mu.Lock()
	defer mu.Unlock()
	if len(resumed) != 1 || resumed[0] != crashSession || h.model.count() != 0 {
		t.Fatalf("resumed = %v, model requests = %d", resumed, h.model.count())
	}
	if nested := completedOutputs(events)[crashSession+"/1"]; nested.Output != execInterrupted {
		t.Fatalf("nested call = %+v", nested)
	}
	if turn := lastTurn(events); turn.Status != statusCompleted {
		t.Fatalf("turn = %+v", turn)
	}
}

func TestRecoverWorkflowTurnWithoutStartInterrupts(t *testing.T) {
	t.Parallel()
	var mu sync.Mutex
	var resumed []string
	h := newHarness(t, withResumer(&mu, &resumed))
	h.seed(workflowTurn()...)
	events := h.recoverSession()
	mu.Lock()
	defer mu.Unlock()
	if turn := lastTurn(events); turn.Status != statusInterrupted || len(resumed) != 0 || h.model.count() != 0 {
		t.Fatalf("turn = %+v, resumed = %v, model requests = %d", turn, resumed, h.model.count())
	}
}

func TestRecoverWorkflowTurnAfterCompletedRunEndsCompleted(t *testing.T) {
	t.Parallel()
	var mu sync.Mutex
	var resumed []string
	h := newHarness(t, withResumer(&mu, &resumed))
	sha, err := h.journal.PutBlob([]byte("1"))
	if err != nil {
		t.Fatal(err)
	}
	h.seed(prefix(workflowTurn(), []event.Event{
		event.ScriptStarted{RunID: crashSession, Entry: "flow.star", SourceBlob: event.BlobRef{Blob: sha, Bytes: 1}, Args: []byte("null")},
		event.ScriptCompleted{RunID: crashSession, OK: true, Value: []byte("1")},
	})...)
	events := h.recoverSession()
	mu.Lock()
	defer mu.Unlock()
	if turn := lastTurn(events); turn.Status != statusCompleted || len(resumed) != 0 || h.model.count() != 0 {
		t.Fatalf("turn = %+v, resumed = %v, model requests = %d", turn, resumed, h.model.count())
	}
}

func TestRecoverModelTurnWithWorkflowPromptCallsTheModel(t *testing.T) {
	t.Parallel()
	var mu sync.Mutex
	var resumed []string
	h := newHarness(t, withResumer(&mu, &resumed))
	h.seed(prefix(workflowTurn(), []event.Event{assistantCall("c1", "read", `{"path":"a.txt"}`)})...)
	h.recoverSession()
	mu.Lock()
	defer mu.Unlock()
	if len(resumed) != 0 || h.model.count() == 0 {
		t.Fatalf("resumed = %v, model requests = %d", resumed, h.model.count())
	}
}
