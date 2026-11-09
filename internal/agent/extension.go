package agent

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/task"
)

var ErrNoTurn = errors.New("agent: a nested call ran outside a turn")

type baseExtension struct{}

func (baseExtension) Tools(builtin []tool.Tool) []tool.Tool { return builtin }

func (baseExtension) Visible(specs []tool.Spec) []tool.Spec { return specs }

func (baseExtension) Direct(string) error { return nil }

func (baseExtension) ToolCall(context.Context, tool.Call) (HookVerdict, error) {
	return HookVerdict{}, nil
}

func (baseExtension) TurnEnd(context.Context, string, string) error { return nil }

type workerKey struct{}

func withWorker(ctx context.Context, w *worker) context.Context {
	return context.WithValue(ctx, workerKey{}, w)
}

func turnWorker(ctx context.Context) (*worker, error) {
	w, ok := ctx.Value(workerKey{}).(*worker)
	if !ok {
		return nil, ErrNoTurn
	}
	return w, nil
}

func quoteJSON(text string) jsontext.Value {
	out, err := jsontext.AppendQuote(nil, text)
	if err != nil {
		out, _ = jsontext.AppendQuote(nil, string(bytes.ToValidUTF8([]byte(text), []byte("\uFFFD"))))
	}
	return out
}

func (t *Thread) Records() ([]event.Seqd, event.State, error) {
	v, err := t.view()
	return v.records, v.state, err
}

func (t *Thread) Append(ctx context.Context, events ...event.Event) error {
	_, err := t.appendEvents(ctx, events...)
	return err
}

func (t *Thread) StoreRaw(value jsontext.Value) (jsontext.Value, *event.BlobRef, error) {
	return t.host.rawPayload(value)
}

func (t *Thread) LoadRaw(value jsontext.Value, blob *event.BlobRef) (jsontext.Value, error) {
	return t.host.loadRaw(value, blob)
}

func (t *Thread) LoadText(text string, blob *event.BlobRef) (string, error) {
	return t.host.loadText(text, blob)
}

func (t *Thread) PutBlob(data []byte) (event.BlobRef, error) {
	sha, err := t.host.journal.PutBlob(data)
	if err != nil {
		return event.BlobRef{}, fmt.Errorf("agent: store blob: %w", err)
	}
	return event.BlobRef{Blob: sha, Bytes: int64(len(data))}, nil
}

func (t *Thread) Blob(ref event.BlobRef) ([]byte, error) {
	return t.host.Blob(&ref)
}

func (t *Thread) RunNestedCall(ctx context.Context, call event.ToolCall) error {
	w, err := turnWorker(ctx)
	if err != nil {
		return err
	}
	v, err := w.t.view()
	if err != nil {
		return err
	}
	pending := approvalFor(v, call.ID)
	if pending.resolved {
		pending.requestID = ""
	}
	return w.runNestedCall(ctx, call, pending.requestID)
}

func (w *worker) runNestedCall(ctx context.Context, call event.ToolCall, requestID string) error {
	item, ok := w.tools.lookup(call.Name)
	if !ok {
		return w.declineCall(ctx, call, requestID, decline("mode:"+string(w.settings.Approval), "Tool "+call.Name+" not found"), nil)
	}
	p, err := w.admit(ctx, &prepared{call: call, tool: item, class: item.Class(), args: call.Args}, requestID)
	if err != nil || p == nil {
		return err
	}
	completed, err := w.execute(ctx, p)
	if err != nil {
		return err
	}
	_, err = w.t.appendEvents(ctx, completed)
	return err
}

func (t *Thread) StartChild(ctx context.Context, callID string, req task.Request) (string, error) {
	select {
	case t.children <- struct{}{}:
	case <-ctx.Done():
		return "", context.Cause(ctx)
	}
	defer func() { <-t.children }()
	child, err := t.childFor(ctx, callID, req)
	if err != nil {
		return "", err
	}
	report, err := ask(child, func(a *actor) childReport { return a.childReport() })
	if err != nil {
		return "", err
	}
	if report.stage == stageNeedsPrompt {
		err = child.Submit(ctx, Input{Kind: InputPrompt, Text: req.Prompt})
	}
	return child.id, err
}

func (t *Thread) ChildText(ctx context.Context, childID string) (string, error) {
	child, err := t.host.Resume(ctx, childID)
	if err != nil {
		return "", err
	}
	return finalText(ctx, child)
}

type childEnd struct {
	running bool
	history childHistory
	damage  error
}

func finalText(ctx context.Context, child *Thread) (string, error) {
	for {
		end, err := ask(child, func(a *actor) childEnd {
			return childEnd{running: a.busy || a.state.TurnID != "", history: scanChild(a.records), damage: a.damage}
		})
		switch {
		case err != nil:
			return "", err
		case end.damage != nil:
			return "", end.damage
		case end.running:
			err = waitTurnEnd(ctx, child)
		case end.history.yield != nil:
			return string(child.yieldResult(*end.history.yield).Output), nil
		default:
			return child.host.loadText(end.history.last.Text, end.history.last.TextBlob)
		}
		if err != nil {
			return "", err
		}
	}
}
