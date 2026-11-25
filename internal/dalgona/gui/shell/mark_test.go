//go:build gui

package shell

import (
	"image"
	"testing"

	"gioui.org/io/input"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/unit"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

func TestBothCommittedMarksDecode(t *testing.T) {
	marks, err := decodeMarks()
	if err != nil {
		t.Fatal(err)
	}
	if marks.large.Width != 120 || marks.small.Width != 120 {
		t.Fatalf("mark canvases %vx / %vx, want the 120-unit viewBox", marks.large.Width, marks.small.Width)
	}
}

func markNodes(t *testing.T, m Mark) []input.SemanticNode {
	t.Helper()
	shaper, err := platform.NewShaper()
	if err != nil {
		t.Fatal(err)
	}
	router := new(input.Router)
	ops := new(op.Ops)
	gtx := layout.Context{Ops: ops, Metric: unit.Metric{PxPerDp: 1, PxPerSp: 1}, Constraints: layout.Exact(image.Pt(200, 200)), Source: router.Source()}
	dims := m.Layout(gtx, Env{Theme: theme.Day, Shaper: shaper})
	if want := image.Pt(int(m.Size), int(m.Size)); dims.Size != want {
		t.Errorf("mark at %v dp measures %v", m.Size, dims.Size)
	}
	router.Frame(ops)
	return router.AppendSemantics(nil)
}

func TestMarkDescribesTheCandyUnlessDecorative(t *testing.T) {
	described := false
	for _, n := range markNodes(t, Mark{Size: 48}) {
		described = described || n.Desc.Label == markDescription
	}
	if !described {
		t.Error("a standalone mark lacks its accessible description")
	}
	for _, n := range markNodes(t, Mark{Size: 20, Decorative: true}) {
		if n.Desc.Label != "" {
			t.Errorf("a decorative mark exposes %q", n.Desc.Label)
		}
	}
}
