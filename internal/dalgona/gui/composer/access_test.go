//go:build gui

package composer

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access/accesstest"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func TestAccessibleComposerExposesAndEditsCanonicalText(t *testing.T) {
	h := newHarness(t, Host{})
	h.ax = accesstest.New()
	h.view.openThread("")
	h.view.drafts[testKey] = state.Draft{Text: "a😀b", Revision: 1}
	h.frame()
	n := h.ax.Find(t, "Message", platform.RoleTextField)
	if n.Value != "a😀b" || n.Selection != (inputtext.Range{Start: 6, End: 6}) || n.Placeholder == "" || n.Bounds.Empty() {
		t.Fatalf("composer value, caret or placeholder missing: %+v", n)
	}
	selection := inputtext.Range{Start: 1, End: 5}
	i := platform.AccessibilityIntent{Node: n.ID, Revision: h.ax.Tree.Snapshot().Revision(), Action: platform.ActionSelectText, Selection: selection}
	if err := h.ax.Tree.Request(i); err != nil {
		t.Fatal(err)
	}
	h.frame()
	if got := h.ax.Find(t, "Message", platform.RoleTextField); got.Selection != selection {
		t.Fatalf("selected bytes %+v, want %+v", got.Selection, selection)
	}
}

func TestAccessibilityEditUsesComposerValidation(t *testing.T) {
	h := newHarness(t, Host{})
	h.ax = accesstest.New()
	h.view.openThread("")
	h.view.drafts[testKey] = state.Draft{Text: "keep", Revision: 1}
	h.frame()
	n := h.ax.Find(t, "Message", platform.RoleTextField)
	i := platform.AccessibilityIntent{Node: n.ID, Revision: h.ax.Tree.Snapshot().Revision(), Action: platform.ActionSetValue, Value: "bad\x00text"}
	if err := h.ax.Tree.Request(i); err != nil {
		t.Fatal(err)
	}
	h.frame()
	if got := h.ax.Find(t, "Message", platform.RoleTextField); got.Value != "keep" {
		t.Fatalf("refused edit changed the composer to %q", got.Value)
	}
	for _, notice := range h.ax.Nodes {
		if notice.Role == platform.RoleError && notice.Target == n.ID && notice.Name != "" {
			return
		}
	}
	t.Fatal("no validation error named the composer field")
}
