package platform

import (
	"slices"
	"testing"
)

func tableSnapshot(t *testing.T, nodes ...SemanticNode) *SemanticSnapshot {
	t.Helper()
	s, err := NewSemanticSnapshot(1, "", append([]SemanticNode{{ID: "doc", Role: RoleDocument}, {ID: "table", Parent: "doc", Role: RoleTable, Name: "Usage"}}, nodes...))
	if err != nil {
		t.Fatal(err)
	}
	return s
}

func cellAt(g tableGrid, row, column int) NodeID {
	if i, ok := g.at(row, column); ok {
		return g.cells[i].id
	}
	return ""
}

func TestTableGridPlacesCellsByRowsAndColumns(t *testing.T) {
	s := tableSnapshot(t,
		SemanticNode{ID: "head", Parent: "table", Role: RoleRow},
		SemanticNode{ID: "h-model", Parent: "head", Role: RoleColumnHeader, Name: "Model"},
		SemanticNode{ID: "h-tokens", Parent: "head", Role: RoleColumnHeader, Name: "Tokens"},
		SemanticNode{ID: "r1", Parent: "table", Role: RoleRow},
		SemanticNode{ID: "r1-model", Parent: "r1", Role: RoleCell, Name: "large"},
		SemanticNode{ID: "r1-tokens", Parent: "r1", Role: RoleCell, Name: "120"},
		SemanticNode{ID: "caption", Parent: "table", Role: RoleStatus, Name: "Two models"},
		SemanticNode{ID: "r2", Parent: "table", Role: RoleRow},
		SemanticNode{ID: "r2-note", Parent: "r2", Role: RoleImage, Name: "chart"},
		SemanticNode{ID: "r2-model", Parent: "r2", Role: RoleCell, Name: "small"},
		SemanticNode{ID: "loose", Parent: "doc", Role: RoleCell, Name: "orphan"},
	)
	g := s.table("table")
	if len(g.rows) != 3 || g.columns != 2 || len(g.cells) != 5 {
		t.Fatalf("grid %d rows x %d columns with %d cells, want 3 x 2 with 5", len(g.rows), g.columns, len(g.cells))
	}
	items := map[[2]int]NodeID{{0, 1}: "h-tokens", {1, 1}: "r1-tokens", {2, 0}: "r2-model", {2, 1}: "", {3, 0}: "", {-1, 0}: ""}
	for at, want := range items {
		if got := cellAt(g, at[0], at[1]); got != want {
			t.Errorf("cell at %v = %q, want %q", at, got, want)
		}
	}
	checkGridColumnHeaders(t, s, g)
}

func checkGridColumnHeaders(t *testing.T, s *SemanticSnapshot, g tableGrid) {
	t.Helper()
	cell, _ := s.Node("r1-tokens")
	place, ok := s.placeOf(cell)
	if !ok || place.table != "table" || place.cell.rows != (tableSpan{1, 1}) || place.cell.columns != (tableSpan{1, 1}) {
		t.Fatalf("cell place %+v %t, want row 1 column 1 of the table", place, ok)
	}
	if got := place.grid.headers(RoleColumnHeader, place.cell.columns, place.cell.id); !slices.Equal(got, []NodeID{"h-tokens"}) {
		t.Errorf("column headers of the cell %v, want h-tokens", got)
	}
	if got := g.everyHeader(RoleColumnHeader); !slices.Equal(got, []NodeID{"h-model", "h-tokens"}) {
		t.Errorf("column headers %v", got)
	}
	loose, _ := s.Node("loose")
	if _, ok := s.placeOf(loose); ok {
		t.Error("a cell outside a table reported a grid place")
	}
}

func TestTableGridHonorsSpansAndRowHeaders(t *testing.T) {
	s := tableSnapshot(t,
		SemanticNode{ID: "r0", Parent: "table", Role: RoleRow},
		SemanticNode{ID: "h-wide", Parent: "r0", Role: RoleColumnHeader, Name: "Usage", ColumnSpan: 2},
		SemanticNode{ID: "h-cost", Parent: "r0", Role: RoleColumnHeader, Name: "Cost"},
		SemanticNode{ID: "r1", Parent: "table", Role: RoleRow},
		SemanticNode{ID: "fast", Parent: "r1", Role: RoleRowHeader, Name: "Fast", RowSpan: 2},
		SemanticNode{ID: "a", Parent: "r1", Role: RoleCell, Name: "1"},
		SemanticNode{ID: "b", Parent: "r1", Role: RoleCell, Name: "2"},
		SemanticNode{ID: "r2", Parent: "table", Role: RoleRow},
		SemanticNode{ID: "c", Parent: "r2", Role: RoleCell, Name: "3", ColumnSpan: 2},
		SemanticNode{ID: "r3", Parent: "table", Role: RoleRow},
		SemanticNode{ID: "deep", Parent: "r3", Role: RoleRowHeader, Name: "Deep", RowSpan: 9},
		SemanticNode{ID: "d", Parent: "r3", Role: RoleCell, Name: "4"},
		SemanticNode{ID: "e", Parent: "r3", Role: RoleCell, Name: "5"},
		SemanticNode{ID: "f", Parent: "r3", Role: RoleCell, Name: "6"},
	)
	g := s.table("table")
	want := map[NodeID][4]int{
		"h-wide": {0, 1, 0, 2}, "h-cost": {0, 1, 2, 1},
		"fast": {1, 2, 0, 1}, "a": {1, 1, 1, 1}, "b": {1, 1, 2, 1},
		"c":    {2, 1, 1, 2},
		"deep": {3, 1, 0, 1}, "d": {3, 1, 1, 1}, "e": {3, 1, 2, 1}, "f": {3, 1, 3, 1},
	}
	for id, w := range want {
		c, ok := g.find(id)
		if got := [4]int{c.rows.first, c.rows.count, c.columns.first, c.columns.count}; !ok || got != w {
			t.Errorf("%s placed at rows %d+%d columns %d+%d, want %v", id, got[0], got[1], got[2], got[3], w)
		}
	}
	if len(g.rows) != 4 || g.columns != 4 {
		t.Fatalf("grid %d x %d, want 4 x 4", len(g.rows), g.columns)
	}
	covered := map[[2]int]NodeID{{2, 0}: "fast", {0, 1}: "h-wide", {2, 2}: "c", {0, 3}: "", {1, 3}: ""}
	for at, id := range covered {
		if got := cellAt(g, at[0], at[1]); got != id {
			t.Errorf("cell at %v is %q, want %q", at, got, id)
		}
	}
	checkSpannedGridHeaders(t, g)
}

func checkSpannedGridHeaders(t *testing.T, g tableGrid) {
	t.Helper()
	c, _ := g.find("c")
	if got := g.headers(RoleColumnHeader, c.columns, c.id); !slices.Equal(got, []NodeID{"h-wide", "h-cost"}) {
		t.Errorf("column headers over the spanned cell %v", got)
	}
	if got := g.headers(RoleRowHeader, c.rows, c.id); !slices.Equal(got, []NodeID{"fast"}) {
		t.Errorf("row headers of the spanned row %v, want the header spanning into it", got)
	}
	if got := g.headers(RoleRowHeader, tableSpan{first: 0, count: 1}, ""); len(got) != 0 {
		t.Errorf("row 0 has row headers %v", got)
	}
	fast, _ := g.find("fast")
	if got := g.headers(RoleRowHeader, fast.rows, fast.id); len(got) != 0 {
		t.Errorf("a row header lists itself or another header: %v", got)
	}
	if got := g.everyHeader(RoleRowHeader); !slices.Equal(got, []NodeID{"fast", "deep"}) {
		t.Errorf("row headers %v", got)
	}
}

func TestTableGridClipsAColumnSpanAtACellFromAbove(t *testing.T) {
	s := tableSnapshot(t,
		SemanticNode{ID: "r0", Parent: "table", Role: RoleRow},
		SemanticNode{ID: "p", Parent: "r0", Role: RoleCell, Name: "p"},
		SemanticNode{ID: "q", Parent: "r0", Role: RoleCell, Name: "q", RowSpan: 2},
		SemanticNode{ID: "r1", Parent: "table", Role: RoleRow},
		SemanticNode{ID: "s", Parent: "r1", Role: RoleCell, Name: "s", ColumnSpan: 3},
		SemanticNode{ID: "t", Parent: "r1", Role: RoleCell, Name: "t"},
	)
	g := s.table("table")
	for id, w := range map[NodeID][2]int{"s": {0, 1}, "t": {2, 1}, "q": {1, 1}} {
		c, _ := g.find(id)
		if got := [2]int{c.columns.first, c.columns.count}; got != w {
			t.Errorf("%s spans columns %v, want %v", id, got, w)
		}
	}
	if got := cellAt(g, 1, 1); got != "q" {
		t.Errorf("row 1 column 1 is %q, want the cell q spanning from row 0", got)
	}
}

func TestCheckSemanticsBoundsCellSpans(t *testing.T) {
	cases := map[string]SemanticNode{
		"negative row span":      {ID: "x", Role: RoleCell, RowSpan: -1},
		"negative column span":   {ID: "x", Role: RoleCell, ColumnSpan: -1},
		"row span over limit":    {ID: "x", Role: RoleCell, RowSpan: 65535},
		"column span over limit": {ID: "x", Role: RoleColumnHeader, ColumnSpan: 65535},
		"span on a row":          {ID: "x", Role: RoleRow, RowSpan: 2},
		"span on a heading":      {ID: "x", Role: RoleHeading, Level: 1, ColumnSpan: 2},
	}
	for name, n := range cases {
		if err := CheckSemantics([]SemanticNode{n}); err == nil {
			t.Errorf("%s: accepted", name)
		}
	}
	if err := CheckSemantics([]SemanticNode{{ID: "x", Role: RoleRowHeader, RowSpan: 65534, ColumnSpan: 1}}); err != nil {
		t.Errorf("the largest span was refused: %v", err)
	}
}
