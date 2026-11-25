//go:build gui

package access_test

import (
	"image"
	"testing"

	"gioui.org/layout"
	"gioui.org/op"

	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access/accesstest"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func draw(t *testing.T, ax *accesstest.Frame, register func(layout.Context), beforePublish func()) {
	t.Helper()
	gtx := ax.Begin(layout.Context{Constraints: layout.Exact(image.Pt(200, 80)), Ops: new(op.Ops)})
	register(gtx)
	if beforePublish != nil {
		beforePublish()
	}
	ax.End(t, gtx)
}

func TestAccessibilityAcceptedActivationSurvivesPublication(t *testing.T) {
	ax := accesstest.New()
	calls := 0
	status := "Ready"
	register := func(gtx layout.Context) {
		n := platform.SemanticNode{ID: "open", Role: platform.RoleButton, Name: "Open", Action: "activate"}
		access.Add(gtx, image.Rect(0, 0, 80, 30), n, access.Actions{Activate: func() { calls++ }})
		access.Add(gtx, image.Rect(0, 30, 80, 60), platform.SemanticNode{ID: "status", Role: platform.RoleStatus, Name: status}, access.Actions{})
	}
	draw(t, ax, register, nil)
	button := ax.Find(t, "Open", platform.RoleButton)
	revision := ax.Tree.Snapshot().Revision()
	status = "Busy"
	draw(t, ax, register, func() { ax.Request(t, button, platform.ActionActivate) })
	if calls != 0 || ax.Tree.Snapshot().Revision() <= revision {
		t.Fatalf("activation ran before the next frame or publication did not advance: %d activations, revision %d", calls, ax.Tree.Snapshot().Revision())
	}
	draw(t, ax, register, nil)
	if calls != 1 {
		t.Fatalf("accepted button ran %d times across publication, want 1", calls)
	}
}

func TestAccessibilityAcceptedSelectionTracksTextRevision(t *testing.T) {
	for _, tc := range []struct {
		name, value string
		want        bool
	}{{"same text", "alpha", true}, {"changed text", "bravo", false}} {
		t.Run(tc.name, func(t *testing.T) {
			ax := accesstest.New()
			selection := new(document.State)
			n := platform.SemanticNode{ID: "text", Role: platform.RoleDocument, Name: "Text", Value: "alpha"}
			register := func(gtx layout.Context) {
				position := func(int, document.Affinity) (document.Position, error) { return document.Position{}, nil }
				access.Add(gtx, image.Rect(0, 0, 80, 30), n, access.Actions{TextPosition: position, Selection: selection})
			}
			draw(t, ax, register, nil)
			old := ax.Find(t, "Text", platform.RoleDocument)
			n.Value, n.Description = tc.value, "Document status changed"
			draw(t, ax, register, func() { ax.Request(t, old, platform.ActionSelectText) })
			draw(t, ax, register, nil)
			if selection.Selected != tc.want {
				t.Fatalf("selection applied = %v, want %v", selection.Selected, tc.want)
			}
		})
	}
}

func TestAccessibilityAcceptedReplacementDoesNotClobberNewText(t *testing.T) {
	t.Run("plain", func(t *testing.T) { checkQueuedReplacement(t, platform.RoleTextField) })
	t.Run("secure", func(t *testing.T) { checkQueuedReplacement(t, platform.RoleSecureField) })
}

func checkQueuedReplacement(t *testing.T, role platform.Role) {
	t.Helper()
	ax := accesstest.New()
	policy := textinput.Policy{Lines: textinput.SingleLine, Limit: textinput.Limit{Unit: textinput.Scalars, Max: 80}}
	editor, err := platform.NewEditor(policy, textinput.Draft{Text: "first"})
	if err != nil {
		t.Fatal(err)
	}
	register := func(gtx layout.Context) {
		n := platform.SemanticNode{ID: "edit", Role: role, Name: "Edit", Action: "edit"}
		access.Add(gtx, image.Rect(0, 0, 80, 30), n, access.Actions{Editor: editor})
	}
	draw(t, ax, register, nil)
	old := ax.Find(t, "Edit", role)
	if err = editor.SetDraft(textinput.Draft{Text: "new typing"}); err != nil {
		t.Fatal(err)
	}
	draw(t, ax, register, func() {
		i := platform.AccessibilityIntent{Node: old.ID, Revision: ax.Tree.Snapshot().Revision(), Action: platform.ActionSetValue, Value: "old replacement"}
		if requestErr := ax.Tree.Request(i); requestErr != nil {
			t.Fatal(requestErr)
		}
	})
	draw(t, ax, register, nil)
	if editor.Draft().Text != "new typing" {
		t.Fatal("stale replacement took effect")
	}
	i := platform.AccessibilityIntent{Node: old.ID, Revision: ax.Tree.Snapshot().Revision(), Action: platform.ActionSetValue, Value: "fresh replacement"}
	if err = ax.Tree.Request(i); err != nil {
		t.Fatal(err)
	}
	draw(t, ax, register, nil)
	if editor.Draft().Text != i.Value {
		t.Fatal("replacement against the current field did not take effect")
	}
}
