//go:build gui

package panel

import (
	"slices"
	"testing"

	"gioui.org/io/semantic"

	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type settingRow struct {
	Key     string `json:"key"`
	Value   any    `json:"value"`
	Options []any  `json:"options"`
}

func sampleSettings(theme string) map[string]any {
	return map[string]any{"settings": []settingRow{
		{Key: "approval", Value: "auto", Options: []any{"auto", "yolo", "ask"}},
		{Key: "verbosity", Value: "medium", Options: []any{"low", "medium", "high"}},
		{Key: "theme", Value: theme, Options: []any{"auto", "auto-soft", "night", "soft-night", "day", "soft-day"}},
		{Key: "icons", Value: "auto", Options: []any{"auto", "unicode", "nerd"}},
		{Key: "statusHide", Value: []string{"cost"}, Options: []any{"effort", "approval", "sandbox", "folder", "queue", "cost", "context", "mark", "model"}},
		{Key: "editReview", Value: false, Options: []any{true, false}},
	}}
}

func settingsPanelWith(t *testing.T, theme string) *harness {
	t.Helper()
	h := newHarness(t)
	h.open(shell.Route{Panel: "settings"})
	if got := h.methods(); !slices.Equal(got, []string{"dalgona/settings/read"}) {
		t.Fatalf("opening Settings sent %q, want only dalgona/settings/read", got)
	}
	h.answer(h.call("dalgona/settings/read"), sampleSettings(theme))
	return h
}

func TestSettingsSaveSendsOnlyChangedPaths(t *testing.T) {
	h := settingsPanelWith(t, "auto")
	h.choose("Night")
	h.clickNode(semantic.CheckBox, "Queue")
	h.clickNode(semantic.CheckBox, "Review edits")
	h.click("Save")
	call := h.call("config/batchWrite")
	edits := params[batchWrite](t, call).Edits
	var paths []string
	for _, e := range edits {
		paths = append(paths, e.KeyPath)
	}
	if !slices.Equal(paths, []string{"theme", "statusHide", "editReview"}) || !call.Operation {
		t.Fatalf("saved paths %q, want theme, statusHide, editReview", paths)
	}
	h.answer(call, map[string]any{"status": "ok"})
	if refresh := submitted[state.RefreshEndpoint](h.intents); len(refresh) != 1 || h.closed != 1 {
		t.Fatalf("success refreshed %v and closed %d", refresh, h.closed)
	}
}

func TestUnknownSettingValueNamesTheFieldAndBlocksSaving(t *testing.T) {
	h := settingsPanelWith(t, "neon")
	h.click("Save")
	h.want("Choose a theme.")
	if slices.Contains(h.methods(), "config/batchWrite") {
		t.Fatal("an unknown theme was saved")
	}
}

func TestSettingsExplainApprovalAndEditReviewAndHideModel(t *testing.T) {
	h := settingsPanelWith(t, "auto")
	h.reject("Open threads keep their existing approval and sandbox.")
	h.choose("Ask")
	h.want("Open threads keep their existing approval and sandbox.")
	if n, ok := h.node(semantic.CheckBox, "Review edits"); !ok || n.Desc.Description != "Sends each edit's masked diff to the decider model." {
		t.Fatalf("edit review lacks its recipient: %+v", n.Desc)
	}
	if _, ok := h.node(semantic.CheckBox, "model"); ok {
		t.Fatal("model is offered as a hidden status item")
	}
}

func TestSettingsFailureKeepsValuesAndLinksOpenTheirPanels(t *testing.T) {
	h := settingsPanelWith(t, "auto")
	h.choose("Day")
	h.click("Save")
	h.fail(h.call("config/batchWrite"), "disk full")
	h.want("Unable to save the settings: disk full")
	if n, ok := h.node(semantic.RadioButton, "Day"); !ok || !n.Desc.Selected {
		t.Fatal("failure lost the chosen theme")
	}
	for _, link := range []struct{ label, panel string }{{"Endpoints", "remote"}, {"MCP servers", "mcp"}, {"Failure reporting", "failures"}} {
		h.click(link.label)
		if last := h.opened[len(h.opened)-1]; last.Panel != link.panel {
			t.Fatalf("%s opened %q", link.label, last.Panel)
		}
	}
}
