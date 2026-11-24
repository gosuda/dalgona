//go:build windows && (amd64 || arm64)

package platform

const uiaRowMajor int32 = 0

func uiaIntAnswer(this *uiaInterface, out *int32, answer func(q uiaQuery) (int32, uintptr)) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnter(this)
	if hr != uiaOK {
		return hr
	}
	value, hr := answer(q)
	*out = value
	return hr
}

func uiaElementsAnswer(this *uiaInterface, out *uintptr, answer func(q uiaQuery) ([]NodeID, uintptr)) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnter(this)
	if hr != uiaOK {
		return hr
	}
	ids, hr := answer(q)
	if hr != uiaOK {
		return hr
	}
	return q.simpleArray(out, ids)
}

func (q uiaQuery) gridPlace() (tablePlace, uintptr) {
	place, ok := q.snapshot.placeOf(q.node)
	if !ok {
		return tablePlace{}, uiaInvalidOperation
	}
	return place, uiaOK
}

func (q uiaQuery) simpleArray(out *uintptr, ids []NodeID) uintptr {
	providers := make([]*uiaInterface, 0, len(ids))
	for _, id := range ids {
		n, _ := q.snapshot.Node(id)
		providers = append(providers, &q.element.bridge.element(id, n.Role).simple)
	}
	return uiaObjectArray(providers, out)
}

func uiaGridGetItem(this *uiaInterface, row, column int32, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnter(this)
	if hr != uiaOK {
		return hr
	}
	grid := q.snapshot.table(q.element.id)
	i, ok := grid.at(int(row), int(column))
	if !ok {
		return uiaInvalidArg
	}
	return q.handOutSimple(out, grid.cells[i].id)
}

func uiaGridRowCount(this *uiaInterface, out *int32) uintptr {
	return uiaIntAnswer(this, out, func(q uiaQuery) (int32, uintptr) {
		return clampInt32(len(q.snapshot.table(q.element.id).rows)), uiaOK
	})
}

func uiaGridColumnCount(this *uiaInterface, out *int32) uintptr {
	return uiaIntAnswer(this, out, func(q uiaQuery) (int32, uintptr) {
		return clampInt32(q.snapshot.table(q.element.id).columns), uiaOK
	})
}

func uiaGridItemAnswer(along func(c tableCell) int) func(this *uiaInterface, out *int32) uintptr {
	return func(this *uiaInterface, out *int32) uintptr {
		return uiaIntAnswer(this, out, func(q uiaQuery) (int32, uintptr) {
			place, hr := q.gridPlace()
			return clampInt32(along(place.cell)), hr
		})
	}
}

var (
	uiaGridItemRow        = uiaGridItemAnswer(func(c tableCell) int { return c.rows.first })
	uiaGridItemColumn     = uiaGridItemAnswer(func(c tableCell) int { return c.columns.first })
	uiaGridItemRowSpan    = uiaGridItemAnswer(func(c tableCell) int { return c.rows.count })
	uiaGridItemColumnSpan = uiaGridItemAnswer(func(c tableCell) int { return c.columns.count })
)

func uiaGridItemContainer(this *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnter(this)
	if hr != uiaOK {
		return hr
	}
	place, hr := q.gridPlace()
	if hr != uiaOK {
		return hr
	}
	return q.handOutSimple(out, place.table)
}

func uiaTableHeaders(header Role) func(this *uiaInterface, out *uintptr) uintptr {
	return func(this *uiaInterface, out *uintptr) uintptr {
		return uiaElementsAnswer(this, out, func(q uiaQuery) ([]NodeID, uintptr) {
			return q.snapshot.table(q.element.id).everyHeader(header), uiaOK
		})
	}
}

func uiaTableItemHeaders(header Role) func(this *uiaInterface, out *uintptr) uintptr {
	return func(this *uiaInterface, out *uintptr) uintptr {
		return uiaElementsAnswer(this, out, func(q uiaQuery) ([]NodeID, uintptr) {
			place, hr := q.gridPlace()
			return place.grid.headers(header, place.cell.along(header), place.cell.id), hr
		})
	}
}

func uiaTableMajor(this *uiaInterface, out *int32) uintptr {
	return uiaIntAnswer(this, out, func(uiaQuery) (int32, uintptr) { return uiaRowMajor, uiaOK })
}
