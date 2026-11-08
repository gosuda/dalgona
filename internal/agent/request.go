package agent

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"strings"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/model"
)

const maxEmptyRetries = 2

var (
	errEmptyResponse = errors.New("empty model response")
	errOverflowTwice = errors.New("context overflow after compaction")
)

type reply struct {
	itemID     string
	contextSeq uint64
	text       strings.Builder
	reasoning  strings.Builder
	calls      []event.ToolCall
	usage      model.Usage
	stop       string
}

type modelFailure struct{ failure model.Error }

func (e modelFailure) Error() string {
	return cmp.Or(e.failure.Message, errorText(e.failure.Err), "model request failed")
}

func errorText(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}

func (e modelFailure) overflow() bool {
	return model.Classify(e.failure.Status, e.failure.Body) == model.Overflow
}

func isOverflow(err error) bool {
	failure, ok := errors.AsType[modelFailure](err)
	return ok && failure.overflow()
}

func (w *worker) callModel(ctx context.Context, v view, messages []model.Message) (*reply, error) {
	overflowed := false
	for {
		result, err := w.requestWithEmptyRetries(ctx, v, messages)
		if !isOverflow(err) {
			return result, err
		}
		if overflowed {
			return nil, errOverflowTwice
		}
		overflowed = true
		if err = w.recoverOverflow(ctx); err != nil {
			return nil, err
		}
		if v, messages, err = w.context(); err != nil {
			return nil, err
		}
	}
}

func (w *worker) requestWithEmptyRetries(ctx context.Context, v view, messages []model.Message) (*reply, error) {
	for attempt := 0; ; attempt++ {
		req, err := w.request(ctx, v, messages)
		if err != nil {
			return nil, err
		}
		result, err := w.stream(ctx, req, v.head())
		if err != nil || !result.empty() {
			return result, err
		}
		if attempt == maxEmptyRetries {
			return nil, errEmptyResponse
		}
		retry := event.ModelRetry{TurnID: w.turnID, Attempt: attempt + 1, Verdict: "retry"}
		if _, err := w.t.appendEvents(ctx, retry); err != nil {
			return nil, err
		}
	}
}

func (r *reply) empty() bool {
	return strings.TrimSpace(r.text.String()) == "" && len(r.calls) == 0
}

func (w *worker) request(ctx context.Context, v view, messages []model.Message) (model.Request, error) {
	_, modelID, err := model.ParseModelID(w.t.created.Model)
	if err != nil {
		return model.Request{}, err
	}
	caps := w.t.host.capabilities(ctx, w.registry, w.t.created.Model)
	return model.Request{
		Model: modelID, System: w.t.system, Messages: messages, Tools: w.t.modelTools,
		Reasoning: model.ClampEffort(w.effort(v), caps.Efforts),
	}, nil
}

func (w *worker) effort(v view) string {
	effort := string(w.t.host.cfg.Config.Reasoning)
	if effort == string(config.ReasoningDefault) {
		effort = ""
	}
	for _, item := range v.records {
		message, ok := item.Event.(event.UserMessage)
		if !ok || message.TurnID != w.turnID || message.Kind != string(InputPrompt) {
			continue
		}
		for _, notice := range message.Notices {
			if w.t.host.keywordEffort(notice.Word) == string(config.ReasoningMax) {
				return string(config.ReasoningMax)
			}
		}
	}
	return effort
}

func (h *Host) keywordEffort(word string) string {
	for _, keyword := range h.keywords {
		if keyword.Word == word {
			return keyword.Effort
		}
	}
	return ""
}

func (w *worker) stream(ctx context.Context, req model.Request, contextSeq uint64) (*reply, error) {
	provider, _, err := model.ParseModelID(w.t.created.Model)
	if err != nil {
		return nil, err
	}
	stream, err := w.registry.Stream(ctx, provider, req)
	if err != nil {
		return nil, err
	}
	result := &reply{itemID: newID("item-"), contextSeq: contextSeq}
	err = w.consume(ctx, stream, result)
	if closeErr := stream.Close(); closeErr != nil {
		w.t.host.log.Debug("agent: close model stream", "thread", w.t.id, "error", closeErr)
	}
	return result, err
}

func (w *worker) consume(ctx context.Context, stream model.Stream, result *reply) error {
	for {
		next, err := stream.Next(ctx)
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return err
		}
		if err := w.apply(result, next); err != nil {
			return err
		}
	}
}

func (w *worker) apply(result *reply, next model.Event) error {
	switch value := next.(type) {
	case model.TextDelta:
		result.text.WriteString(value.Text)
		w.t.publish(Update{Kind: UpdateTextDelta, ItemID: result.itemID, Delta: value.Text})
	case model.ReasoningDelta:
		result.reasoning.WriteString(value.Text)
		w.t.publish(Update{Kind: UpdateReasoningDelta, ItemID: result.itemID, Delta: value.Text})
	case model.ToolCall:
		result.calls = append(result.calls, event.ToolCall{
			ID: cmp.Or(value.ID, newID("call-")), Name: value.Name, Args: value.Args, Freeform: value.Freeform, Resolved: value.Resolved,
		})
	case model.Usage:
		result.usage = value
	case model.Done:
		result.stop = value.StopReason
	case model.Error:
		return modelFailure{failure: value}
	}
	return nil
}

func (w *worker) journalAssistant(ctx context.Context, result *reply) error {
	text, textBlob, err := w.t.host.textPayload(result.text.String())
	if err != nil {
		return err
	}
	reasoning, reasoningBlob, err := w.t.host.textPayload(result.reasoning.String())
	if err != nil {
		return err
	}
	if err = w.storeCallArgs(result.calls); err != nil {
		return err
	}
	stop := "stop"
	if len(result.calls) > 0 {
		stop = "tool_calls"
	}
	message := event.AssistantMessage{
		ItemID: result.itemID, TurnID: w.turnID, Text: text, TextBlob: textBlob, Reasoning: reasoning, ReasoningBlob: reasoningBlob,
		ToolCalls: result.calls, StopReason: cmp.Or(result.stop, stop), ContextSeq: result.contextSeq,
		Usage: event.Usage{Input: result.usage.Input, Output: result.usage.Output, CacheRead: result.usage.CacheRead, CacheWrite: result.usage.CacheWrite},
	}
	_, err = w.t.appendEvents(ctx, message)
	return err
}

func (w *worker) storeCallArgs(calls []event.ToolCall) error {
	for i, call := range calls {
		if len(call.Args) == 0 {
			call.Args = jsontext.Value("{}")
		}
		args, blob, err := w.t.host.rawPayload(call.Args)
		if err != nil {
			return err
		}
		calls[i].Args, calls[i].ArgsBlob = args, blob
	}
	return nil
}

func (h *Host) capabilities(ctx context.Context, registry *model.Registry, id string) model.Model {
	provider, modelID, err := model.ParseModelID(id)
	if err != nil {
		return model.Model{ID: id, Context: model.ContextWindow(id, 0, h.contextOverrides())}
	}
	for _, item := range h.providerModels(ctx, registry, provider) {
		if item.ID == modelID || item.ID == id {
			item.Context = model.ContextWindow(id, item.Context, h.contextOverrides())
			return item
		}
	}
	return model.Model{ID: modelID, Context: model.ContextWindow(id, 0, h.contextOverrides())}
}

func (h *Host) providerModels(ctx context.Context, registry *model.Registry, provider string) []model.Model {
	h.mu.Lock()
	cached, ok := h.models[provider]
	h.mu.Unlock()
	if ok {
		return cached
	}
	adapter, found := registry.Provider(provider)
	if !found {
		return nil
	}
	models, err := adapter.Models(ctx)
	if err != nil {
		h.log.Warn("agent: list provider models", "provider", provider, "error", err)
		return nil
	}
	h.mu.Lock()
	h.models[provider] = models
	h.mu.Unlock()
	return models
}

func (h *Host) contextOverrides() map[string]int {
	overrides := make(map[string]int, len(h.cfg.Config.Models))
	for id, spec := range h.cfg.Config.Models {
		overrides[id] = spec.ContextWindow
	}
	return overrides
}

func (h *Host) ResolveRole(ctx context.Context, role, threadModel string) (string, error) {
	switch role {
	case "smol":
		role = cmp.Or(h.cfg.Config.Roles.Smol, role)
	case "slow":
		role = cmp.Or(h.cfg.Config.Roles.Slow, role)
	}
	provider, _, err := model.ParseModelID(threadModel)
	if err != nil {
		return "", err
	}
	registry, err := h.cfg.Providers(nil)
	if err != nil {
		return "", err
	}
	models := h.providerModels(ctx, registry, provider)
	offered := make([]string, 0, len(models))
	for _, item := range models {
		offered = append(offered, provider+"/"+item.ID)
	}
	resolved, message := model.ResolveRole(role, threadModel, offered)
	if message != "" {
		h.logOnce(message, message)
	}
	return resolved, nil
}
