//go:build linux

package platform

type atspiPoint struct {
	Row, Column int32
}

func (o *atspiObject) grid() tableGrid { return o.snap.table(o.node.ID) }

func (o *atspiObject) cellRef(id NodeID, found bool) atspiRef {
	if !found {
		return atspiRef{Path: atspiNullPath}
	}
	return o.ref(id)
}

func (o *atspiObject) firstRef(ids []NodeID) atspiRef {
	if len(ids) == 0 {
		return atspiRef{Path: atspiNullPath}
	}
	return o.ref(ids[0])
}

func atspiCellAt(g tableGrid, row, column int32) (tableCell, bool) {
	i, ok := g.at(int(row), int(column))
	if !ok {
		return tableCell{}, false
	}
	return g.cells[i], true
}

func atspiCellIndexed(g tableGrid, index int32) (tableCell, bool) {
	if index < 0 || int(index) >= len(g.cells) {
		return tableCell{}, false
	}
	return g.cells[index], true
}

func atspiGridAnswer(answer func(o *atspiObject, g tableGrid, row, column int32) any) func(o *atspiObject, args []any) ([]any, error) {
	return func(o *atspiObject, args []any) ([]any, error) {
		return []any{answer(o, o.grid(), args[0].(int32), args[1].(int32))}, nil
	}
}

func atspiIndexAnswer(answer func(c tableCell) int32) func(o *atspiObject, args []any) ([]any, error) {
	return func(o *atspiObject, args []any) ([]any, error) {
		c, ok := atspiCellIndexed(o.grid(), args[0].(int32))
		if !ok {
			return []any{int32(-1)}, nil
		}
		return []any{answer(c)}, nil
	}
}

func atspiSpanAt(along func(c tableCell) tableSpan) func(o *atspiObject, g tableGrid, row, column int32) any {
	return func(_ *atspiObject, g tableGrid, row, column int32) any {
		c, ok := atspiCellAt(g, row, column)
		if !ok {
			return int32(0)
		}
		return clampInt32(along(c).count)
	}
}

func atspiHeaderAt(header Role) func(o *atspiObject, args []any) ([]any, error) {
	return func(o *atspiObject, args []any) ([]any, error) {
		line := tableSpan{first: int(args[0].(int32)), count: 1}
		return []any{o.firstRef(o.grid().headers(header, line, ""))}, nil
	}
}

func cellRows(c tableCell) tableSpan { return c.rows }

func cellColumns(c tableCell) tableSpan { return c.columns }

var atspiTableProperties = map[string]atspiProperty{
	"version":  atspiConstant(uint32(1)),
	"NRows":    atspiRead(func(o *atspiObject) any { return clampInt32(len(o.grid().rows)) }),
	"NColumns": atspiRead(func(o *atspiObject) any { return clampInt32(o.grid().columns) }),
}

var atspiTableMembers = map[string]atspiMember{
	"GetAccessibleAt": {in: []any{int32(0), int32(0)}, out: []any{atspiRef{}}, call: atspiGridAnswer(func(o *atspiObject, g tableGrid, row, column int32) any {
		c, ok := atspiCellAt(g, row, column)
		return o.cellRef(c.id, ok)
	})},
	"GetIndexAt": {in: []any{int32(0), int32(0)}, out: []any{int32(0)}, call: atspiGridAnswer(func(_ *atspiObject, g tableGrid, row, column int32) any {
		i, _ := g.at(int(row), int(column))
		return clampInt32(i)
	})},
	"GetRowExtentAt":    {in: []any{int32(0), int32(0)}, out: []any{int32(0)}, call: atspiGridAnswer(atspiSpanAt(cellRows))},
	"GetColumnExtentAt": {in: []any{int32(0), int32(0)}, out: []any{int32(0)}, call: atspiGridAnswer(atspiSpanAt(cellColumns))},
	"GetRowAtIndex":     {in: []any{int32(0)}, out: []any{int32(0)}, call: atspiIndexAnswer(func(c tableCell) int32 { return clampInt32(c.rows.first) })},
	"GetColumnAtIndex":  {in: []any{int32(0)}, out: []any{int32(0)}, call: atspiIndexAnswer(func(c tableCell) int32 { return clampInt32(c.columns.first) })},
	"GetColumnHeader":   {in: []any{int32(0)}, out: []any{atspiRef{}}, call: atspiHeaderAt(RoleColumnHeader)},
	"GetRowHeader":      {in: []any{int32(0)}, out: []any{atspiRef{}}, call: atspiHeaderAt(RoleRowHeader)},
	"GetRowColumnExtentsAtIndex": {in: []any{int32(0)}, out: []any{false, int32(0), int32(0), int32(0), int32(0), false}, call: func(o *atspiObject, args []any) ([]any, error) {
		c, ok := atspiCellIndexed(o.grid(), args[0].(int32))
		return []any{ok, clampInt32(c.rows.first), clampInt32(c.columns.first), clampInt32(c.rows.count), clampInt32(c.columns.count), false}, nil
	}},
}

func (o *atspiObject) cellPlace() (tablePlace, bool) {
	if o.kind != atspiNodeKind || o.snap == nil {
		return tablePlace{}, false
	}
	return o.snap.placeOf(o.node)
}

func (o *atspiObject) cell() (tablePlace, error) {
	place, ok := o.cellPlace()
	if !ok {
		return tablePlace{}, atspiInvalid("%q is not a cell of a table row; ask GetInterfaces first", o.node.ID)
	}
	return place, nil
}

func atspiCellProperty(value func(o *atspiObject, at tablePlace) any) atspiProperty {
	return func(o *atspiObject) (any, error) {
		at, err := o.cell()
		if err != nil {
			return nil, err
		}
		return value(o, at), nil
	}
}

func atspiCellAnswer(value func(o *atspiObject, at tablePlace) any) func(o *atspiObject, _ []any) ([]any, error) {
	return func(o *atspiObject, _ []any) ([]any, error) {
		at, err := o.cell()
		if err != nil {
			return nil, err
		}
		return []any{value(o, at)}, nil
	}
}

func atspiCellHeaders(header Role) func(o *atspiObject, at tablePlace) any {
	return func(o *atspiObject, at tablePlace) any {
		return o.refs(at.grid.headers(header, at.cell.along(header), at.cell.id))
	}
}

var atspiTableCellProperties = map[string]atspiProperty{
	"version":    atspiConstant(uint32(1)),
	"RowSpan":    atspiCellProperty(func(_ *atspiObject, at tablePlace) any { return clampInt32(at.cell.rows.count) }),
	"ColumnSpan": atspiCellProperty(func(_ *atspiObject, at tablePlace) any { return clampInt32(at.cell.columns.count) }),
	"Position": atspiCellProperty(func(_ *atspiObject, at tablePlace) any {
		return atspiPoint{Row: clampInt32(at.cell.rows.first), Column: clampInt32(at.cell.columns.first)}
	}),
	"Table": atspiCellProperty(func(o *atspiObject, at tablePlace) any { return o.ref(at.table) }),
}

var atspiTableCellMembers = map[string]atspiMember{
	"GetRowColumnSpan": {out: []any{false, int32(0), int32(0), int32(0), int32(0)}, call: func(o *atspiObject, _ []any) ([]any, error) {
		at, err := o.cell()
		if err != nil {
			return nil, err
		}
		c := at.cell
		return []any{true, clampInt32(c.rows.first), clampInt32(c.columns.first), clampInt32(c.rows.count), clampInt32(c.columns.count)}, nil
	}},
	"GetColumnHeaderCells": {out: []any{[]atspiRef{}}, call: atspiCellAnswer(atspiCellHeaders(RoleColumnHeader))},
	"GetRowHeaderCells":    {out: []any{[]atspiRef{}}, call: atspiCellAnswer(atspiCellHeaders(RoleRowHeader))},
}
