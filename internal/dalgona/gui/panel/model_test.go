//go:build gui

package panel

import (
	"slices"
	"testing"

	"gioui.org/io/semantic"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func threeEfforts() []rpc.ReasoningEffortOption {
	return []rpc.ReasoningEffortOption{{ReasoningEffort: "low"}, {ReasoningEffort: "medium"}, {ReasoningEffort: "high"}}
}

var sampleModels = []rpc.Model{
	{ID: "openai/slowpoke", DisplayName: "Slowpoke", Description: "Careful", DefaultReasoningEffort: "high", SupportedReasoningEfforts: threeEfforts(), Presets: []string{"slow"}},
	{ID: "openai/secret", DisplayName: "Secret", Hidden: true, Presets: []string{"default"}},
	{ID: "openai/tiny", DisplayName: "Tiny", Description: "Small and quick", DefaultReasoningEffort: "low", SupportedReasoningEfforts: []rpc.ReasoningEffortOption{{ReasoningEffort: "low"}}, Presets: []string{"smol"}},
	{ID: "openai/main", DisplayName: "Main", Description: "Everyday", DefaultReasoningEffort: "medium", SupportedReasoningEfforts: threeEfforts(), Presets: []string{"default"}},
}

func modelPanelWith(t *testing.T, models []rpc.Model, cfg rpc.ConfigValues) *harness {
	t.Helper()
	h := newHarness(t)
	h.open(shell.Route{Panel: "model"})
	h.answer(h.call("model/list"), rpc.ModelListResponse{Data: models})
	h.answer(h.call("config/read"), rpc.ConfigReadResponse{Config: cfg})
	return h
}

func TestModelPanelOffersOnlyVisibleModelsWithDescriptions(t *testing.T) {
	h := modelPanelWith(t, sampleModels, rpc.ConfigValues{Model: "openai/main", ModelReasoningEffort: "medium"})
	h.want("Slowpoke", "Tiny", "Main")
	for _, label := range h.radios() {
		if label == "Secret" {
			t.Fatal("a hidden model is selectable")
		}
	}
	if n, ok := h.node(semantic.RadioButton, "Tiny"); !ok || n.Desc.Description != "Small and quick" {
		t.Fatalf("model description missing: %+v", n.Desc)
	}
}

func TestPresetsFollowDefaultSmolSlowAndSetModelWithEffort(t *testing.T) {
	h := modelPanelWith(t, sampleModels, rpc.ConfigValues{Model: "openai/main", ModelReasoningEffort: "medium"})
	var presets []string
	for _, label := range h.radios() {
		if slices.Contains([]string{"default · Main · medium", "smol · Tiny · low", "slow · Slowpoke · high"}, label) {
			presets = append(presets, label)
		}
	}
	if want := []string{"default · Main · medium", "smol · Tiny · low", "slow · Slowpoke · high"}; !slices.Equal(presets, want) {
		t.Fatalf("presets %q, want %q", presets, want)
	}
	h.choose("slow · Slowpoke · high")
	h.click("Use model")
	edits := params[batchWrite](t, h.call("config/batchWrite")).Edits
	want := []settingEdit{replaceEdit("model", "openai/slowpoke"), replaceEdit("model_reasoning_effort", "high")}
	if !slices.EqualFunc(edits, want, func(a, b settingEdit) bool {
		return a.KeyPath == b.KeyPath && a.Value == b.Value && a.MergeStrategy == b.MergeStrategy
	}) {
		t.Fatalf("batch edits %+v, want %+v", edits, want)
	}
}

func TestChangingModelReplacesAnUndeclaredEffort(t *testing.T) {
	h := modelPanelWith(t, sampleModels, rpc.ConfigValues{Model: "openai/main", ModelReasoningEffort: "high"})
	h.choose("Tiny")
	h.click("Use model")
	edits := params[batchWrite](t, h.call("config/batchWrite")).Edits
	if len(edits) != 2 || edits[1].Value != "low" {
		t.Fatalf("Tiny kept an undeclared effort: %+v", edits)
	}
}

func TestModelSaveRefreshesAndFailureKeepsTheChoice(t *testing.T) {
	h := modelPanelWith(t, sampleModels, rpc.ConfigValues{Model: "openai/main", ModelReasoningEffort: "medium"})
	h.choose("Slowpoke")
	h.click("Use model")
	call := h.call("config/batchWrite")
	if !call.Operation {
		t.Fatal("model save is not an operation")
	}
	h.fail(call, "settings changed")
	h.want("Unable to save the model selection: settings changed")
	if got := h.selected(); got != "Slowpoke" {
		t.Fatalf("failure changed the selection to %q", got)
	}
	h.click("Use model")
	h.answer(h.call("config/batchWrite"), rpc.ConfigWriteResponse{Status: "ok"})
	if refresh := submitted[state.RefreshEndpoint](h.intents); len(refresh) != 1 || refresh[0].Endpoint != local || h.closed != 1 {
		t.Fatalf("success refreshed %v and closed %d times", refresh, h.closed)
	}
}

func TestEmptyModelListExplainsLogin(t *testing.T) {
	h := modelPanelWith(t, []rpc.Model{{ID: "x", Hidden: true}}, rpc.ConfigValues{})
	h.want("No models. Log in to a provider to list its models.")
	h.click("Log in")
	if len(h.opened) != 1 || h.opened[0].Panel != "login" || h.opened[0].Thread.Endpoint != local {
		t.Fatalf("Log in opened %v", h.opened)
	}
}
