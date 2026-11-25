package vtstate

import "slices"

type row struct {
	cells   []Cell
	wrapped bool
}

func newRow(columns int, fill Cell) row {
	cells := make([]Cell, columns)
	for x := range cells {
		cells[x] = fill
	}
	return row{cells: cells}
}

func (r row) splitAt(x int) {
	if x <= 0 || x >= len(r.cells) || !r.cells[x].Continuation() {
		return
	}
	head := r.cells[x-1].blanked()
	r.cells[x-1] = head
	r.cells[x] = head
}

func (r row) set(x int, c Cell) {
	if x < 0 || x >= len(r.cells) {
		return
	}
	r.splitAt(x)
	r.splitAt(x + 1)
	if c.Width < 2 {
		r.cells[x] = c
		return
	}
	if x+1 >= len(r.cells) {
		r.cells[x] = c.blanked()
		return
	}
	r.splitAt(x + 2)
	r.cells[x] = c
	r.cells[x+1] = Cell{}
}

func (r row) fill(from, to int, c Cell) {
	from = max(from, 0)
	to = min(to, len(r.cells))
	if from >= to {
		return
	}
	r.splitAt(from)
	r.splitAt(to)
	for x := from; x < to; x++ {
		r.cells[x] = c
	}
}

func (r row) insertBlanks(x, n, right int, c Cell) {
	n = min(n, right-x)
	r.splitAt(x)
	r.splitAt(right - n)
	r.splitAt(right)
	copy(r.cells[x+n:right], r.cells[x:right-n])
	for i := x; i < x+n; i++ {
		r.cells[i] = c
	}
}

func (r row) deleteCells(x, n, right int, c Cell) {
	n = min(n, right-x)
	r.splitAt(x)
	r.splitAt(x + n)
	r.splitAt(right)
	copy(r.cells[x:right-n], r.cells[x+n:right])
	for i := right - n; i < right; i++ {
		r.cells[i] = c
	}
}

func (r *row) resize(columns int) {
	if columns < len(r.cells) {
		r.splitAt(columns)
		r.cells = slices.Clip(r.cells[:columns])
		return
	}
	for len(r.cells) < columns {
		r.cells = append(r.cells, blankCell(Color{}))
	}
}

func (r row) blank() bool {
	for _, c := range r.cells {
		if c.Text != " " && c.Text != "" {
			return false
		}
	}
	return true
}

func (r row) trimmed() row {
	end := len(r.cells)
	for end > 0 && r.cells[end-1].isBlank() {
		end--
	}
	return row{cells: slices.Clone(r.cells[:end]), wrapped: r.wrapped}
}

func (r row) textBytes() int {
	n := 1
	for i := range r.cells {
		n += len(r.cells[i].Text)
		if link := r.cells[i].Link; link != nil {
			n += len(link.URI) + len(link.ID)
		}
	}
	return n
}
