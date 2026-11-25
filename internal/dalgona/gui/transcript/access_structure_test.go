//go:build gui

package transcript

import (
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access/accesstest"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestAccessibilityTranscriptPublishesRichStructure(t *testing.T) {
	withTranscriptDeps(t, "", mathDeps(t), semanticTurn, func(t *testing.T, h *harness) {
		h.size.Y = 900
		h.ax = accesstest.New()
		installFigureOutcome(t, h)
		checkSemanticLists(t, h.ax)
		checkSemanticTable(t, h.ax)
		checkSemanticFigure(t, h)
		checkSemanticFormula(t, h.ax)
		checkSemanticImage(t, h.ax)
	})
}

func semanticTurn(d *daemon) []rpc.HistoryTurn {
	text := "1. Outer\n   - Inner\n2. Next\n\n| Name | Count |\n| --- | --- |\n| apples | 7 |\n\n" +
		"```d2\nSource -> Result\n```\n\n$$\\frac{x}{2}$$"
	img := map[string]any{"type": "image", "name": "map.png", "altText": "Map of the work area"}
	return []rpc.HistoryTurn{frozenTurn("u1", rpc.StatusCompleted,
		d.header("u", rpc.KindUser, map[string]any{"content": []any{img}}, fieldData{name: "text", data: "Reference image"}),
		d.header("a", rpc.KindAgent, nil, fieldData{name: "text", data: text}),
	)}
}

func installFigureOutcome(t *testing.T, h *harness) {
	t.Helper()
	sup, err := figure.NewSupervisor(figure.SupervisorConfig{Fonts: figureFonts()})
	if err != nil {
		t.Fatal(err)
	}
	sup.Close()
	h.tr.deps.Figures = figure.NewHub(sup, nil)
	h.frame()
	doc := h.tr.panes[h.key].rows[identity(h.key, "u1", "a")].doc
	for i := range doc.Len() {
		b := doc.Block(i)
		if b.Kind == document.KindFigure {
			data := &figure.List{Title: "Work flow", Width: 120, Height: 90, Labels: []string{"Source", "Result"}}
			s := h.tr.figureState(b)
			if s.key == "" {
				t.Fatal("the visible diagram did not establish its render request key")
			}
			s.err = nil
			s.canvas = figure.NewCanvas(data, h.shaper, figure.Faces{Sans: platform.ProseFace, Mono: platform.CodeFace})
			h.frame()
			return
		}
	}
	t.Fatal("fixture did not parse a diagram")
}

func checkSemanticLists(t *testing.T, f *accesstest.Frame) {
	t.Helper()
	s := f.Tree.Snapshot()
	outer := f.Find(t, "Ordered list", platform.RoleList)
	children := s.Children(outer.ID)
	if len(children) != 2 || !listItemContains(s, children[0], "Outer") || !listItemContains(s, children[1], "Next") {
		t.Fatalf("ordered list children did not preserve the two items: %v", children)
	}
	inner := f.Find(t, "List", platform.RoleList)
	items := s.Children(inner.ID)
	if inner.Parent != children[0] || len(items) != 1 || !listItemContains(s, items[0], "Inner") {
		t.Fatalf("nested list parent %q, items %v, expected a list inside the first item's content", inner.Parent, items)
	}
}

func listItemContains(s *platform.SemanticSnapshot, id platform.NodeID, text string) bool {
	item, _ := s.Node(id)
	return item.Role == platform.RoleListItem && slices.ContainsFunc(s.Children(id), func(child platform.NodeID) bool {
		n, _ := s.Node(child)
		return strings.TrimSpace(n.Value) == text
	})
}

func checkSemanticTable(t *testing.T, f *accesstest.Frame) {
	t.Helper()
	s := f.Tree.Snapshot()
	table := f.Find(t, "Table", platform.RoleTable)
	rows := s.Children(table.ID)
	header := f.Find(t, "Count", platform.RoleColumnHeader)
	cell := f.Find(t, "7", platform.RoleCell)
	if len(rows) != 2 || header.Parent != rows[0] || cell.Parent != rows[1] {
		t.Fatalf("header %q and cell %q do not belong to separate table rows %v", header.Parent, cell.Parent, rows)
	}
	col := slices.Index(s.Children(rows[0]), header.ID)
	if col != 1 || col != slices.Index(s.Children(rows[1]), cell.ID) || cell.RowSpan != 1 || cell.ColumnSpan != 1 || cell.Value != "7" {
		t.Fatalf("Count header and cell are not in the same column or cell span/text is wrong: %+v", cell)
	}
}

func checkSemanticFigure(t *testing.T, h *harness) {
	t.Helper()
	f := h.ax
	fig := f.Find(t, "Work flow", platform.RoleFigure)
	s := f.Tree.Snapshot()
	if got := childValues(s, fig.ID); !slices.Equal(got, []string{"d2", "Source", "Result", "Source -> Result\n"}) {
		t.Fatalf("figure children lost caption, labels or source in reading order: %q", got)
	}
	button := f.Find(t, "Show source", platform.RoleButton)
	if button.Parent != fig.ID {
		t.Fatalf("source button belongs to %q instead of figure %q", button.Parent, fig.ID)
	}
	f.Request(t, button, platform.ActionActivate)
	h.frame()
	if toggle := f.Find(t, "Show figure", platform.RoleButton); toggle.ID != button.ID {
		t.Fatalf("the figure source switch changed identity from %q to %q", button.ID, toggle.ID)
	}
}

func childValues(s *platform.SemanticSnapshot, id platform.NodeID) []string {
	var values []string
	for _, child := range s.Children(id) {
		n, _ := s.Node(child)
		if n.Value != "" {
			values = append(values, n.Value)
		}
	}
	return values
}

func checkSemanticFormula(t *testing.T, f *accesstest.Frame) {
	t.Helper()
	formula := f.Find(t, "Formula", platform.RoleMath)
	s := f.Tree.Snapshot()
	if !strings.Contains(formula.Description, "fraction") || !slices.Contains(childValues(s, formula.ID), `\frac{x}{2}`) {
		t.Fatalf("formula lacks source or speech: %+v", formula)
	}
	for _, child := range s.Children(formula.ID) {
		part, _ := s.Node(child)
		if part.Role == platform.RoleMath && part.Description == "fraction" && slices.Equal(childValues(s, child), []string{"x", "2"}) {
			return
		}
	}
	t.Fatal("formula lacks navigable numerator and denominator")
}

func checkSemanticImage(t *testing.T, f *accesstest.Frame) {
	t.Helper()
	img := f.Find(t, "map.png", platform.RoleImage)
	button := f.Find(t, "Hide preview for map.png", platform.RoleButton)
	if img.Description != "Map of the work area" || img.ID != button.Parent || button.Action == "" {
		t.Fatalf("image lacks alternative text or a related preview control: image %+v, control %+v", img, button)
	}
}
