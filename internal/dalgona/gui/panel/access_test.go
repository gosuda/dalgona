//go:build gui

package panel

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access/accesstest"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestSettingsPanelAccessibilityExposesAndTogglesASetting(t *testing.T) {
	h := settingsPanelWith(t, "night")
	h.ax = accesstest.New()
	h.frame()
	night := h.ax.Find(t, "Night", platform.RoleRadioButton)
	if !night.Checked || night.Bounds.Empty() {
		t.Fatalf("the current theme is not checked at its location: %+v", night)
	}
	edit := h.ax.Find(t, "Review edits", platform.RoleCheckBox)
	if edit.Checked || edit.Bounds.Empty() || edit.Disabled {
		t.Fatalf("unchecked editing toggle has incorrect state: %+v", edit)
	}
	h.ax.Request(t, edit, platform.ActionActivate)
	h.frame()
	if got := h.ax.Find(t, "Review edits", platform.RoleCheckBox); !got.Checked || got.ID != edit.ID {
		t.Fatalf("activation did not check the same toggle: %+v", got)
	}
}
