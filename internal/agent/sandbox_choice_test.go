package agent

import (
	"path/filepath"
	"strings"
	"testing"
	"testing/synctest"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	asktool "github.com/gosuda/dalgona/internal/tool/ask"
)

const sandboxCauseText = "Landlock is not available on this kernel"

func withUnavailableSandbox(approval config.Approval) func(*harness) {
	return func(h *harness) {
		h.cfg.Config.Approval = approval
		h.cfg.Config.Sandbox = config.SandboxWorkspaceWrite
		h.cfg.SandboxCause = func() string { return sandboxCauseText }
	}
}

func pendingSandboxQuestions(updates <-chan Update) []QuestionPrompt {
	var out []QuestionPrompt
	for {
		select {
		case update := <-updates:
			if update.Kind == UpdateQuestion && !update.Question.Resolved && isSandboxQuestion(update.Question) {
				out = append(out, update.Question)
			}
		default:
			return out
		}
	}
}

func isSandboxQuestion(prompt QuestionPrompt) bool {
	return len(prompt.Questions) == 1 && prompt.Questions[0].Header == "Sandbox"
}

func sandboxChoices(events []event.Seqd) []event.SandboxChosen {
	var out []event.SandboxChosen
	for _, item := range events {
		if chosen, ok := item.Event.(event.SandboxChosen); ok {
			out = append(out, chosen)
		}
	}
	return out
}

func answerSandbox(t *testing.T, thread *Thread, prompt QuestionPrompt) {
	t.Helper()
	reply := asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{sandboxAccept}}}
	if err := thread.AnswerQuestion(prompt.RequestID, reply); err != nil {
		t.Fatalf("AnswerQuestion: %v", err)
	}
}

func TestUnavailableSandboxAsksOnceAndRecordsTheChoice(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withUnavailableSandbox(config.ApprovalAsk))
	h.model.push(calls(call{id: "c1", name: "exec", args: `{"command":"echo one"}`}, call{id: "c2", name: "exec", args: `{"command":"echo two"}`}), text("done"))
	thread := h.start()
	updates := thread.Subscribe()
	h.submit(thread, "run two commands")
	prompt := nextQuestionUpdate(t, updates, false)
	if !isSandboxQuestion(prompt) {
		t.Fatalf("first question = %+v, want the sandbox choice", prompt.Questions)
	}
	answerSandbox(t, thread, prompt)
	for range 2 {
		approval := nextApprovalPrompt(t, updates)
		if err := thread.Answer(approval.RequestID, ReplyDecline); err != nil {
			t.Fatalf("Answer: %v", err)
		}
	}
	h.waitIdle(thread)
	if again := pendingSandboxQuestions(updates); len(again) != 0 {
		t.Fatalf("sandbox question asked %d more times, want none", len(again))
	}
	chosen := sandboxChoices(h.events(thread.ID()))
	if len(chosen) != 1 || chosen[0].Choice != event.SandboxChoiceAccept || chosen[0].Cause != sandboxCauseText {
		t.Fatalf("sandbox choices = %+v, want one accept for %q", chosen, sandboxCauseText)
	}
}

func TestUnavailableSandboxDeclinesWithoutAClient(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withUnavailableSandbox(config.ApprovalAsk))
	events := runCalls(t, h, call{id: "c1", name: "exec", args: `{"command":"echo one"}`})
	want := "declined: no client could choose how to run without a sandbox (" + sandboxCauseText + "); set sandbox = \"off\" to run without one"
	if got := callOutputs(h.model.request(1))["c1"]; !strings.Contains(got, want) {
		t.Fatalf("exec output = %q, want it to contain %q", got, want)
	}
	if chosen := sandboxChoices(events); len(chosen) != 0 {
		t.Fatalf("sandbox choices = %+v, want none journaled", chosen)
	}
	if started(events, "c1") {
		t.Fatal("declined exec started a tool call")
	}
	runs, err := filepath.Glob(filepath.Join(h.home, "sessions", "*", "runs", "*"))
	if err != nil {
		t.Fatal(err)
	}
	if len(runs) != 0 {
		t.Fatalf("declined exec left %d running processes", len(runs))
	}
}

func TestCancelledSandboxChoiceStartsNoProcess(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withUnavailableSandbox(config.ApprovalAsk))
	h.model.push(calls(call{id: "c1", name: "exec", args: `{"command":"echo one"}`}), text("done"))
	thread := h.start()
	updates := thread.Subscribe()
	h.submit(thread, "run one command")
	prompt := nextQuestionUpdate(t, updates, false)
	if !isSandboxQuestion(prompt) {
		t.Fatalf("first question = %+v, want the sandbox choice", prompt.Questions)
	}
	if err := thread.AnswerQuestion(prompt.RequestID, asktool.Reply{Outcome: asktool.OutcomeCancelled}); err != nil {
		t.Fatalf("AnswerQuestion cancel: %v", err)
	}
	h.waitIdle(thread)
	if started(h.events(thread.ID()), "c1") {
		t.Fatal("cancelled sandbox choice started the exec tool")
	}
	runs, err := filepath.Glob(filepath.Join(h.home, "sessions", "*", "runs", "*"))
	if err != nil {
		t.Fatal(err)
	}
	if len(runs) != 0 {
		t.Fatalf("cancelled sandbox choice left %d running processes", len(runs))
	}
}

func TestAcceptSendsEveryCommandToApproval(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withUnavailableSandbox(config.ApprovalAuto))
	h.model.push(calls(call{id: "c1", name: "exec", args: `{"command":"echo one"}`}), calls(call{id: "c2", name: "exec", args: `{"command":"echo one"}`}), text("done"))
	thread := h.start()
	updates := thread.Subscribe()
	h.submit(thread, "run the same command twice")
	answerSandbox(t, thread, nextQuestionUpdate(t, updates, false))
	first := nextApprovalPrompt(t, updates)
	if err := thread.Answer(first.RequestID, ReplyAcceptSession); err != nil {
		t.Fatalf("Answer accept for session: %v", err)
	}
	second := nextApprovalPrompt(t, updates)
	if err := thread.Answer(second.RequestID, ReplyDecline); err != nil {
		t.Fatalf("Answer decline: %v", err)
	}
	h.waitIdle(thread)
}

func TestSandboxQuestionKeepsEscapeLookingCallID(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withUnavailableSandbox(config.ApprovalAsk))
	h.model.push(calls(call{id: "c%41", name: "exec", args: `{"command":"echo one"}`}), text("done"))
	thread := h.start()
	updates := thread.Subscribe()
	h.submit(thread, "run one command")
	prompt := nextQuestionUpdate(t, updates, false)
	if prompt.CallID != "c%41" {
		t.Fatalf("sandbox question call id = %q, want %q", prompt.CallID, "c%41")
	}
	if err := thread.AnswerQuestion(prompt.RequestID, asktool.Reply{Outcome: asktool.OutcomeCancelled}); err != nil {
		t.Fatalf("AnswerQuestion cancel: %v", err)
	}
	h.waitIdle(thread)
}

func TestExplicitSandboxOffNeverAsks(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withUnavailableSandbox(config.ApprovalAsk), func(h *harness) { h.cfg.Config.Sandbox = config.SandboxOff })
	h.model.push(calls(call{id: "c1", name: "exec", args: `{"command":"echo one"}`}), text("done"))
	thread := h.start()
	updates := thread.Subscribe()
	h.submit(thread, "run one command")
	approval := nextApprovalPrompt(t, updates)
	if err := thread.Answer(approval.RequestID, ReplyDecline); err != nil {
		t.Fatalf("Answer: %v", err)
	}
	h.waitIdle(thread)
	if asked := pendingSandboxQuestions(updates); len(asked) != 0 {
		t.Fatalf("sandbox question asked with sandbox off: %+v", asked)
	}
	if chosen := sandboxChoices(h.events(thread.ID())); len(chosen) != 0 {
		t.Fatalf("sandbox choices = %+v, want none with sandbox off", chosen)
	}
}

func TestSandboxCancelDeclinesAndAsksAgain(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withUnavailableSandbox(config.ApprovalAsk))
	h.model.push(calls(call{id: "c1", name: "exec", args: `{"command":"echo one"}`}), calls(call{id: "c2", name: "exec", args: `{"command":"echo two"}`}), text("done"))
	thread := h.start()
	updates := thread.Subscribe()
	h.submit(thread, "run two commands")
	first := nextQuestionUpdate(t, updates, false)
	if err := thread.AnswerQuestion(first.RequestID, asktool.Reply{Outcome: asktool.OutcomeCancelled}); err != nil {
		t.Fatalf("AnswerQuestion cancel: %v", err)
	}
	second := nextQuestionUpdate(t, updates, false)
	if second.RequestID == first.RequestID {
		t.Fatalf("cancelled question asked again under the same request id %s", first.RequestID)
	}
	if chosen := sandboxChoices(h.events(thread.ID())); len(chosen) != 0 {
		t.Fatalf("sandbox choices = %+v after a cancelled question, want none journaled", chosen)
	}
	answerSandbox(t, thread, second)
	approval := nextApprovalPrompt(t, updates)
	if err := thread.Answer(approval.RequestID, ReplyDecline); err != nil {
		t.Fatalf("Answer: %v", err)
	}
	h.waitIdle(thread)
	if got := callOutputs(h.model.request(1))["c1"]; got != "declined: no sandbox choice was made" {
		t.Fatalf("cancelled command output = %q", got)
	}
	chosen := sandboxChoices(h.events(thread.ID()))
	if len(chosen) != 1 || chosen[0].Choice != event.SandboxChoiceAccept {
		t.Fatalf("sandbox choices = %+v, want exactly the accept from the second question", chosen)
	}
}

func TestSiblingsShareOneSandboxQuestion(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		h := newHarness(t, withUnavailableSandbox(config.ApprovalAsk))
		thread := h.start()
		updates := thread.Subscribe()
		replies := make(chan asktool.Reply, 2)
		for _, id := range []string{"c1", "c2"} {
			go func() {
				reply, _ := thread.askSandbox(t.Context(), id, sandboxCauseText, "ask")
				replies <- reply
			}()
		}
		synctest.Wait()
		asked := pendingSandboxQuestions(updates)
		if len(asked) != 1 {
			t.Fatalf("sandbox questions = %d, want one shared question", len(asked))
		}
		answerSandbox(t, thread, asked[0])
		for range 2 {
			if reply := <-replies; reply.Outcome != asktool.OutcomeAnswered {
				t.Fatalf("sibling reply = %+v, want the shared answer", reply)
			}
		}
	})
}

func TestChildrenShareOneSandboxQuestionThroughTheGate(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		h := newHarness(t, withUnavailableSandbox(config.ApprovalAuto))
		script := newChildScript(map[string][]response{
			"child a": {calls(call{id: "xa", name: "exec", args: `{"command":"echo a"}`}), text("a done")},
			"child b": {calls(call{id: "xb", name: "exec", args: `{"command":"echo b"}`}), text("b done")},
		})
		h.model.route = script.route
		h.model.push(spawnTwo(), text("parent done"))
		parent := h.start()
		updates := parent.Subscribe()
		h.submit(parent, "spawn")
		synctest.Wait()
		asked := pendingSandboxQuestions(updates)
		if len(asked) != 1 {
			t.Fatalf("parent sandbox questions = %d, want one shared question", len(asked))
		}
		answerSandbox(t, parent, asked[0])
		for range 2 {
			approval := nextApprovalPrompt(t, updates)
			if err := parent.Answer(approval.RequestID, ReplyDecline); err != nil {
				t.Fatalf("Answer decline: %v", err)
			}
		}
		h.waitIdle(parent)
	})
}

func eventSeq(events []event.Seqd, match func(event.Event) bool) uint64 {
	for _, item := range events {
		if match(item.Event) {
			return item.Seq
		}
	}
	return 0
}

func TestAcceptChoiceIsJournaledAfterTheApprovalAnswer(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withUnavailableSandbox(config.ApprovalAsk))
	h.model.push(calls(call{id: "c1", name: "exec", args: `{"command":"echo one"}`}), text("done"))
	thread := h.start()
	updates := thread.Subscribe()
	h.submit(thread, "run one command")
	answerSandbox(t, thread, nextQuestionUpdate(t, updates, false))
	approval := nextApprovalPrompt(t, updates)
	if chosen := sandboxChoices(h.events(thread.ID())); len(chosen) != 0 {
		t.Fatalf("sandbox choices = %+v while the approval request is open, want none journaled", chosen)
	}
	if err := thread.Answer(approval.RequestID, ReplyDecline); err != nil {
		t.Fatalf("Answer: %v", err)
	}
	h.waitIdle(thread)
	events := h.events(thread.ID())
	chosen := sandboxChoices(events)
	if len(chosen) != 1 || chosen[0].Choice != event.SandboxChoiceAccept || chosen[0].RequestID != approval.RequestID {
		t.Fatalf("sandbox choices = %+v, want one accept for request %s", chosen, approval.RequestID)
	}
	requested := eventSeq(events, func(e event.Event) bool {
		value, ok := e.(event.ApprovalRequested)
		return ok && value.RequestID == approval.RequestID
	})
	journaled := eventSeq(events, func(e event.Event) bool {
		_, ok := e.(event.SandboxChosen)
		return ok
	})
	if requested == 0 || journaled <= requested {
		t.Fatalf("approval.requested at seq %d, sandbox.chosen at seq %d, want the choice after the request", requested, journaled)
	}
}

func TestDangerousChoiceIsJournaledWhenTheQuestionIsAnswered(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withUnavailableSandbox(config.ApprovalAsk))
	h.model.push(calls(call{id: "c1", name: "exec", args: `{"command":"echo one"}`}), text("done"))
	thread := h.start()
	updates := thread.Subscribe()
	h.submit(thread, "run one command")
	prompt := nextQuestionUpdate(t, updates, false)
	reply := asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{sandboxDangerous}}}
	if err := thread.AnswerQuestion(prompt.RequestID, reply); err != nil {
		t.Fatalf("AnswerQuestion: %v", err)
	}
	approval := nextApprovalPrompt(t, updates)
	chosen := sandboxChoices(h.events(thread.ID()))
	if len(chosen) != 1 || chosen[0].Choice != event.SandboxChoiceDangerous || chosen[0].RequestID != approval.RequestID {
		t.Fatalf("sandbox choices = %+v while the approval request is open, want one dangerously_accept for %s", chosen, approval.RequestID)
	}
	if err := thread.Answer(approval.RequestID, ReplyDecline); err != nil {
		t.Fatalf("Answer: %v", err)
	}
	h.waitIdle(thread)
}

func sandboxOpenTurn() []event.Event {
	return []event.Event{
		event.TurnStarted{TurnID: "t"},
		event.UserMessage{ItemID: "u", TurnID: "t", Text: "run", Kind: "prompt", Sandbox: string(config.SandboxWorkspaceWrite)},
	}
}

func TestRecoverOpenSandboxQuestionAsksAgain(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withUnavailableSandbox(config.ApprovalAsk))
	h.seed(prefix(sandboxOpenTurn(), []event.Event{
		assistantCall("c1", "exec", `{"command":"echo one"}`),
		event.ApprovalRequested{RequestID: "r-c1", CallID: "c1", Summary: "echo one", Class: "exec"},
	})...)
	events := h.recoverSession()
	want := "declined: no client could choose how to run without a sandbox (" + sandboxCauseText + "); set sandbox = \"off\" to run without one"
	if got := completedOutputs(events)["c1"].Output; got != want {
		t.Fatalf("recovered output = %q, want %q", got, want)
	}
	if chosen := sandboxChoices(events); len(chosen) != 0 {
		t.Fatalf("recovery journaled sandbox choices %+v, want none", chosen)
	}
}

func TestRecoverUsesTheJournaledSandboxChoice(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withUnavailableSandbox(config.ApprovalAsk))
	h.seed(prefix([]event.Event{event.SandboxChosen{RequestID: "r-c0", Choice: event.SandboxChoiceAccept, Cause: sandboxCauseText}}, sandboxOpenTurn(), []event.Event{
		assistantCall("c1", "exec", `{"command":"echo one"}`),
		event.ApprovalRequested{RequestID: "r-c1", CallID: "c1", Summary: "echo one", Class: "exec"},
	})...)
	events := h.recoverSession()
	if got := completedOutputs(events)["c1"].Output; got != declineNoClient {
		t.Fatalf("recovered output = %q, want the approval no-client text %q", got, declineNoClient)
	}
	if chosen := sandboxChoices(events); len(chosen) != 1 || chosen[0].RequestID != "r-c0" {
		t.Fatalf("sandbox choices = %+v, want only the seeded choice", chosen)
	}
}
