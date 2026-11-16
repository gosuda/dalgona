package agent

import (
	"context"
	"slices"
	"strings"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

const externalPrompt = "dalgona run "

type resumingExtension struct {
	baseExtension
	t       *Thread
	mu      *sync.Mutex
	resumed *[]string
}

func (r resumingExtension) MatchExternalTurn(ctx context.Context, turnID string) (bool, error) {
	v, err := r.t.view()
	if err != nil {
		return false, err
	}
	matched := false
	for _, item := range v.records {
		if vetoedTurn(item, turnID) {
			return false, nil
		}
		if hit, ok := r.promptedTurn(item, turnID); ok {
			matched = hit
		}
	}
	return matched, nil
}

func vetoedTurn(item event.Seqd, turnID string) bool {
	value, ok := item.Event.(event.AssistantMessage)
	return ok && value.TurnID == turnID
}

func (r resumingExtension) promptedTurn(item event.Seqd, turnID string) (bool, bool) {
	value, ok := item.Event.(event.UserMessage)
	if !ok || value.TurnID != turnID || value.Kind != string(InputPrompt) || !value.Verbatim {
		return false, false
	}
	text, err := r.t.host.loadText(value.Text, value.TextBlob)
	return err == nil && strings.HasPrefix(text, externalPrompt), true
}

func (r resumingExtension) ResumeExternalTurn(ctx context.Context) error {
	w, err := turnWorker(ctx)
	if err != nil {
		return err
	}
	v, err := r.t.view()
	if err != nil {
		return err
	}
	if _, ok := v.state.Scripts[r.t.id]; !ok {
		for _, item := range v.records {
			if done, ok := item.Event.(event.ScriptCompleted); ok && done.RunID == r.t.id {
				return nil
			}
		}
		return ErrExternalTurnInterrupted
	}
	if err := w.recoverNested(ctx, v, r.t.id+"/"); err != nil {
		return err
	}
	r.mu.Lock()
	*r.resumed = append(*r.resumed, r.t.id)
	r.mu.Unlock()
	return r.t.Append(ctx, event.ScriptCompleted{RunID: r.t.id, OK: true, Value: []byte("1")})
}

func withResumer(mu *sync.Mutex, resumed *[]string) func(*harness) {
	return func(h *harness) {
		h.cfg.Extension = func(t *Thread) Extension { return resumingExtension{t: t, mu: mu, resumed: resumed} }
	}
}

func workflowTurn() []event.Event {
	return []event.Event{
		event.TurnStarted{TurnID: "t"},
		event.UserMessage{ItemID: "u", TurnID: "t", Text: externalPrompt + "flow.star", Kind: "prompt", Verbatim: true},
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

func TestNestedCellCallsStayOutOfModelRequests(t *testing.T) {
	t.Parallel()
	cell := &nestedCell{calls: cellCalls()}
	h := cellHarness(t, cell, false, judgeFor(&judgeStub{}))
	thread := h.start()
	cell.thread = thread
	h.submit(thread, "run the cell")
	h.waitIdle(thread)
	if h.model.count() != 2 {
		t.Fatalf("requests = %d, want 2", h.model.count())
	}
	outputs := callOutputs(h.model.request(1))
	if len(outputs) != 1 {
		t.Fatalf("function call outputs = %v, want only the cell result", outputIDs(outputs))
	}
	if _, ok := outputs["run-1"]; !ok {
		t.Fatalf("function call outputs = %v, want the cell result", outputIDs(outputs))
	}
}

func outputIDs(outputs map[string]string) []string {
	ids := make([]string, 0, len(outputs))
	for id := range outputs {
		ids = append(ids, id)
	}
	slices.Sort(ids)
	return ids
}
