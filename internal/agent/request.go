package agent

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/model"
)

const maxEmptyRetries = 2

const (
	providerModelsTimeout = 30 * time.Second
	negativeModelsTTL     = time.Minute
)

type modelCache struct {
	models  []model.Model
	expires time.Time
}

var (
	errEmptyResponse = errors.New("empty model response")
	errOverflowTwice = errors.New("context overflow after compaction")
	errModelsTimeout = errors.New("agent: list provider models timed out")
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
		if v, messages, err = w.context(ctx); err != nil {
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
	_, modelID, err := model.ParseModelID(w.settings.Model)
	if err != nil {
		return model.Request{}, err
	}
	caps := w.t.host.capabilities(ctx, w.registry, w.settings.Model)
	w.contextWindow = caps.Context
	return model.Request{
		Model: modelID, System: w.t.system, Messages: messages, Tools: w.tools.specs,
		Reasoning: model.ClampEffort(w.effort(v), caps.Efforts),
	}, nil
}

func (w *worker) effort(v view) string {
	configured := cmp.Or(string(w.settings.Effort), string(w.t.host.Config().Reasoning))
	if configured == string(config.ReasoningDefault) {
		configured = ""
	}
	prompt := turnPrompt(v, w.turnID)
	for _, notice := range prompt.Notices {
		if w.t.host.keywordEffort(notice.Word) == string(config.ReasoningMax) {
			return string(config.ReasoningMax)
		}
	}
	return cmp.Or(prompt.Effort, configured)
}

func turnPrompt(v view, turnID string) event.UserMessage {
	for _, item := range v.records {
		message, ok := item.Event.(event.UserMessage)
		if ok && message.TurnID == turnID && message.Kind == string(InputPrompt) {
			return message
		}
	}
	return event.UserMessage{}
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
	provider, _, err := model.ParseModelID(w.settings.Model)
	if err != nil {
		return nil, err
	}
	w.flushInBand(context.WithoutCancel(ctx), contextSeq)
	stream, err := w.openStream(ctx, provider, req)
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
			Signature: value.Signature, RedactedThinking: value.RedactedThinking,
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
	calls := result.calls
	if w.inBand != nil {
		pending := make([]event.ToolCall, 0, len(result.calls))
		for _, call := range result.calls {
			if !w.inBand.resolved(call.ID) {
				pending = append(pending, call)
			}
		}
		calls = pending
	}
	stop := "stop"
	if len(calls) > 0 {
		stop = "tool_calls"
	}
	message := event.AssistantMessage{
		ItemID: result.itemID, TurnID: w.turnID, Text: text, TextBlob: textBlob, Reasoning: reasoning, ReasoningBlob: reasoningBlob,
		ToolCalls: calls, StopReason: cmp.Or(result.stop, stop), ContextSeq: result.contextSeq, ContextWindow: w.contextWindow,
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
	preset, _ := model.Capabilities(provider, modelID)
	reported := model.Model{ID: modelID, Context: preset.Context}
	for _, item := range h.providerModels(ctx, registry, provider) {
		if item.ID == modelID || item.ID == id {
			reported = item
		}
	}
	if len(reported.Efforts) == 0 {
		reported.Efforts = preset.Efforts
	}
	reported.Context = model.ContextWindow(id, cmp.Or(reported.Context, preset.Context), h.contextOverrides())
	return reported
}

func (h *Host) providerModels(ctx context.Context, registry *model.Registry, provider string) []model.Model {
	if models, ok := h.cachedModels(provider); ok {
		return models
	}
	adapter, found := registry.Provider(provider)
	if !found {
		return nil
	}
	models := h.lookupModels(ctx, adapter, provider)
	if len(models) == 0 {
		if ctx.Err() == nil {
			h.cacheModels(provider, nil, h.clock().Add(negativeModelsTTL))
		}
		return nil
	}
	return models
}

func (h *Host) cachedModels(provider string) ([]model.Model, bool) {
	h.mu.Lock()
	cached, ok := h.models[provider]
	now := h.clock()
	h.mu.Unlock()
	if !ok {
		return nil, false
	}
	if !cached.expires.IsZero() && !now.Before(cached.expires) {
		return nil, false
	}
	return cached.models, true
}

func (h *Host) lookupModels(ctx context.Context, adapter model.Provider, provider string) []model.Model {
	out, err, _ := h.modelsGroup.Do(provider, func() (any, error) {
		lookupCtx, cancel := context.WithTimeoutCause(ctx, providerModelsTimeout, errModelsTimeout)
		defer cancel()
		models, lerr := adapter.Models(lookupCtx)
		if lerr != nil {
			return nil, lerr
		}
		if len(models) == 0 {
			return []model.Model{}, nil
		}
		h.cacheModels(provider, models, time.Time{})
		return models, nil
	})
	if err != nil {
		h.log.Warn("agent: list provider models", "provider", provider, "error", err)
	}
	models, _ := out.([]model.Model)
	return models
}

func (h *Host) cacheModels(provider string, models []model.Model, expires time.Time) {
	h.mu.Lock()
	h.models[provider] = modelCache{models: models, expires: expires}
	h.mu.Unlock()
}

func (h *Host) contextOverrides() map[string]int {
	models := h.Config().Models
	overrides := make(map[string]int, len(models))
	for id, spec := range models {
		overrides[id] = spec.ContextWindow
	}
	return overrides
}

func (h *Host) ResolveRole(ctx context.Context, role, threadModel string) (string, error) {
	switch role {
	case "smol":
		role = cmp.Or(h.Config().Roles.Smol, role)
	case "slow":
		role = cmp.Or(h.Config().Roles.Slow, role)
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
