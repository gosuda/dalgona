package agent

import (
	"context"
	"fmt"
	"net/url"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
	asktool "github.com/gosuda/dalgona/internal/tool/ask"
)

const (
	sandboxAccept    = "Accept"
	sandboxDangerous = "Dangerously accept"
	sandboxNoChoice  = "declined: no sandbox choice was made"
)

type sandboxAsk struct {
	done  chan struct{}
	reply asktool.Reply
	err   error
}

func (w *worker) sandboxFallback(ctx context.Context, gc gateCall, requestID string) (verdict, bool) {
	cause := w.t.host.cfg.SandboxCause()
	if !w.sandboxApplies(gc, cause) {
		return verdict{}, false
	}
	switch w.sandboxChoice() {
	case event.SandboxChoiceAccept:
		return w.humanSkippingGrants(ctx, gc, requestID), true
	case event.SandboxChoiceDangerous:
		return verdict{}, false
	}
	return w.askSandboxChoice(ctx, gc, requestID, cause)
}

func (w *worker) sandboxApplies(gc gateCall, cause string) bool {
	return gc.class == tool.ClassExec && !gc.escalate && w.settings.Sandbox == config.SandboxWorkspaceWrite &&
		w.t.created.Runner == "local" && w.t.created.Advisor == "" && cause != ""
}

func (w *worker) askSandboxChoice(ctx context.Context, gc gateCall, requestID, cause string) (verdict, bool) {
	reply, err := w.sandboxAsker().askSandbox(ctx, url.PathEscape(gc.call.ID), cause, string(w.settings.Approval))
	if err == nil && reply.Outcome == asktool.OutcomeNoClient {
		return decline("sandbox", fmt.Sprintf("declined: no client could choose how to run without a sandbox (%s); set sandbox = \"off\" to run without one", cause)), true
	}
	choice, ok := sandboxChoiceFrom(reply)
	if err != nil || !ok {
		return decline("sandbox", sandboxNoChoice), true
	}
	return w.recordSandboxChoice(ctx, gc, requestID, cause, choice)
}

func (w *worker) recordSandboxChoice(ctx context.Context, gc gateCall, requestID, cause, choice string) (verdict, bool) {
	if _, err := w.t.appendEvents(ctx, event.SandboxChosen{RequestID: requestID, Choice: choice, Cause: cause}); err != nil {
		return decline("sandbox", sandboxNoChoice), true
	}
	if choice == event.SandboxChoiceDangerous {
		return verdict{}, false
	}
	return w.humanSkippingGrants(ctx, gc, requestID), true
}

func sandboxChoiceFrom(reply asktool.Reply) (string, bool) {
	if reply.Outcome != asktool.OutcomeAnswered || len(reply.Answers) != 1 || len(reply.Answers[0]) != 1 {
		return "", false
	}
	switch reply.Answers[0][0] {
	case sandboxAccept:
		return event.SandboxChoiceAccept, true
	case sandboxDangerous:
		return event.SandboxChoiceDangerous, true
	}
	return "", false
}

func (w *worker) sandboxChoice() string {
	v, err := w.t.view()
	if err != nil {
		return ""
	}
	return v.state.SandboxChoice.Choice
}

func (w *worker) sandboxAsker() *Thread {
	if w.t.created.Advisor != "" {
		return w.t
	}
	if parent, open := w.t.host.loadedThread(w.t.created.Parent); open {
		return parent
	}
	return w.t
}

func (t *Thread) askSandbox(ctx context.Context, callID, cause, approval string) (asktool.Reply, error) {
	t.sandboxMu.Lock()
	if pending := t.sandboxPending; pending != nil {
		t.sandboxMu.Unlock()
		select {
		case <-pending.done:
			return pending.reply, pending.err
		case <-ctx.Done():
			return asktool.Reply{Outcome: asktool.OutcomeInterrupted}, nil
		}
	}
	pending := &sandboxAsk{done: make(chan struct{})}
	t.sandboxPending = pending
	t.sandboxMu.Unlock()
	warning := "Sandbox unavailable: " + cause + ". Choose how commands in this thread run."
	_, _ = ask(t, func(a *actor) bool {
		a.publish(Update{Kind: UpdateWarning, Thread: a.t.id, Warning: warning})
		return true
	})
	pending.reply, pending.err = t.Ask(ctx, callID, sandboxQuestions(cause, approval))
	t.sandboxMu.Lock()
	t.sandboxPending = nil
	t.sandboxMu.Unlock()
	close(pending.done)
	return pending.reply, pending.err
}

func sandboxQuestions(cause, approval string) []asktool.Question {
	return []asktool.Question{{
		Header: "Sandbox",
		Text:   "The harness sandbox is not available: " + cause + ". How should commands in this thread run?",
		Options: []asktool.Option{
			{Label: sandboxAccept, Description: "Run without a sandbox. Every command asks for your approval first."},
			{Label: sandboxDangerous, Description: "Run without a sandbox under approval mode " + approval + "."},
		},
		NoOther: true,
	}}
}

func (t *Thread) SandboxFallback() (cause, choice string, err error) {
	settings, err := t.Settings()
	if err != nil {
		return "", "", err
	}
	if settings.Sandbox != config.SandboxWorkspaceWrite || t.created.Runner != "local" || t.created.Advisor != "" {
		return "", "", nil
	}
	cause = t.host.cfg.SandboxCause()
	if cause == "" {
		return "", "", nil
	}
	v, err := t.view()
	if err != nil {
		return "", "", err
	}
	switch v.state.SandboxChoice.Choice {
	case event.SandboxChoiceAccept:
		choice = "accept"
	case event.SandboxChoiceDangerous:
		choice = "dangerouslyAccept"
	}
	return cause, choice, nil
}
