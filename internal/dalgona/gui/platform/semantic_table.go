package platform

import (
	"cmp"
	"math"
	"slices"
)

const spanLimit = 65534

func clampInt32(v int) int32 {
	if v > math.MaxInt32 {
		return math.MaxInt32
	}
	if v < math.MinInt32 {
		return math.MinInt32
	}
	return int32(v)
}

func (r Role) cell() bool {
	return r == RoleCell || r == RoleColumnHeader || r == RoleRowHeader
}

type tableSpan struct {
	first, count int
}

func (t tableSpan) overlaps(u tableSpan) bool {
	return t.first < u.first+u.count && u.first < t.first+t.count
}

type tableCell struct {
	id      NodeID
	role    Role
	rows    tableSpan
	columns tableSpan
}

func (c tableCell) covers(row, column int) bool {
	return c.rows.overlaps(tableSpan{first: row, count: 1}) && c.columns.overlaps(tableSpan{first: column, count: 1})
}

func (c tableCell) along(header Role) tableSpan {
	if header == RoleRowHeader {
		return c.rows
	}
	return c.columns
}

type tableGrid struct {
	rows    []NodeID
	cells   []tableCell
	columns int
}

type tablePlace struct {
	table NodeID
	grid  tableGrid
	cell  tableCell
}

func (s *SemanticSnapshot) childrenWith(parent NodeID, roles ...Role) []NodeID {
	var matched []NodeID
	for _, id := range s.Children(parent) {
		if n, _ := s.Node(id); slices.Contains(roles, n.Role) {
			matched = append(matched, id)
		}
	}
	return matched
}

func (s *SemanticSnapshot) table(id NodeID) tableGrid {
	g := tableGrid{rows: s.childrenWith(id, RoleRow)}
	var spanning []tableCell
	for r, row := range g.rows {
		spanning = slices.DeleteFunc(spanning, func(c tableCell) bool { return c.rows.first+c.rows.count <= r })
		for _, c := range g.place(s, r, row, spanning) {
			if c.rows.count > 1 {
				spanning = append(spanning, c)
			}
		}
		slices.SortFunc(spanning, func(a, b tableCell) int { return cmp.Compare(a.columns.first, b.columns.first) })
	}
	return g
}

func (g *tableGrid) place(s *SemanticSnapshot, r int, row NodeID, spanning []tableCell) []tableCell {
	first, column, k := len(g.cells), 0, 0
	for _, id := range s.childrenWith(row, RoleCell, RoleColumnHeader, RoleRowHeader) {
		n, _ := s.Node(id)
		for ; k < len(spanning) && spanning[k].columns.first <= column; k++ {
			column = max(column, spanning[k].columns.first+spanning[k].columns.count)
		}
		width := max(n.ColumnSpan, 1)
		if k < len(spanning) {
			width = min(width, spanning[k].columns.first-column)
		}
		height := min(max(n.RowSpan, 1), len(g.rows)-r)
		g.cells = append(g.cells, tableCell{id: id, role: n.Role, rows: tableSpan{first: r, count: height}, columns: tableSpan{first: column, count: width}})
		column += width
	}
	g.columns = max(g.columns, column)
	return g.cells[first:]
}

func (g tableGrid) at(row, column int) (int, bool) {
	for i, c := range g.cells {
		if c.covers(row, column) {
			return i, true
		}
	}
	return -1, false
}

func (g tableGrid) find(id NodeID) (tableCell, bool) {
	for _, c := range g.cells {
		if c.id == id {
			return c, true
		}
	}
	return tableCell{}, false
}

func (g tableGrid) headers(header Role, over tableSpan, except NodeID) []NodeID {
	var ids []NodeID
	for _, c := range g.cells {
		if c.role == header && c.id != except && c.along(header).overlaps(over) {
			ids = append(ids, c.id)
		}
	}
	return ids
}

func (g tableGrid) everyHeader(header Role) []NodeID {
	return g.headers(header, tableSpan{first: 0, count: max(len(g.rows), g.columns)}, "")
}

func (s *SemanticSnapshot) placeOf(n SemanticNode) (tablePlace, bool) {
	row, _ := s.Node(n.Parent)
	table, _ := s.Node(row.Parent)
	if !n.Role.cell() || row.Role != RoleRow || table.Role != RoleTable {
		return tablePlace{}, false
	}
	grid := s.table(table.ID)
	cell, ok := grid.find(n.ID)
	return tablePlace{table: table.ID, grid: grid, cell: cell}, ok
}
