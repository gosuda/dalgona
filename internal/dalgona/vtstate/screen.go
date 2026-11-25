package vtstate

import "slices"

type region struct {
	top    int
	bottom int
	left   int
	right  int
}

func (r region) contains(x, y int) bool {
	return x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

type cursor struct {
	x           int
	y           int
	wrapPending bool
}

type savedCursor struct {
	cursor
	style    Style
	link     *Link
	origin   bool
	charsets charsets
}

type screen struct {
	rows    []row
	history *scrollback
	cursor  cursor
	saved   savedCursor
	scroll  region
}

func newScreen(columns, rows int, history *scrollback) screen {
	s := screen{rows: make([]row, rows), history: history}
	for y := range s.rows {
		s.rows[y] = newRow(columns, blankCell(Color{}))
	}
	s.reset()
	return s
}

func (s *screen) reset() {
	s.clear()
	s.cursor = cursor{}
	s.saved = savedCursor{charsets: defaultCharsets()}
	s.scroll = s.bounds()
}

func (s *screen) columns() int {
	return len(s.rows[0].cells)
}

func (s *screen) height() int {
	return len(s.rows)
}

func (s *screen) bounds() region {
	return region{bottom: s.height(), right: s.columns()}
}

func (s *screen) fullWidth() bool {
	return s.scroll.left == 0 && s.scroll.right == s.columns()
}

func (s *screen) clear() {
	for y := range s.rows {
		s.rows[y].fill(0, s.columns(), blankCell(Color{}))
		s.rows[y].wrapped = false
	}
}

func (s *screen) fillArea(x0, y0, x1, y1 int, c Cell) {
	columns := s.columns()
	for y := max(y0, 0); y < min(y1, s.height()); y++ {
		s.rows[y].fill(x0, x1, c)
		if x1 >= columns {
			s.rows[y].wrapped = false
		}
	}
}

func (s *screen) setCursor(x, y int, margins bool) {
	if margins {
		y = clamp(s.scroll.top+y, s.scroll.top, s.scroll.bottom-1)
		x = clamp(s.scroll.left+x, s.scroll.left, s.scroll.right-1)
	} else {
		y = clamp(y, 0, s.height()-1)
		x = clamp(x, 0, s.columns()-1)
	}
	s.cursor = cursor{x: x, y: y}
}

func (s *screen) moveCursor(dx, dy int) {
	area := s.scroll
	if s.cursor.x < area.left {
		area.left = 0
	}
	if s.cursor.x >= area.right {
		area.right = s.columns()
	}
	if !area.contains(s.cursor.x, s.cursor.y) {
		area = s.bounds()
	}
	s.cursor = cursor{
		x: clamp(s.cursor.x+dx, area.left, area.right-1),
		y: clamp(s.cursor.y+dy, area.top, area.bottom-1),
	}
}

func (s *screen) insertLines(n int, blank Cell) bool {
	if n <= 0 || !s.scroll.contains(s.cursor.x, s.cursor.y) {
		return false
	}
	s.shiftDown(s.cursor.y, n, blank)
	return true
}

func (s *screen) deleteLines(n int, blank Cell) bool {
	if n <= 0 || !s.scroll.contains(s.cursor.x, s.cursor.y) {
		return false
	}
	s.removeLines(s.cursor.y, n, blank)
	return true
}

func (s *screen) scrollUp(n int, blank Cell) {
	if n > 0 {
		s.removeLines(s.scroll.top, n, blank)
	}
}

func (s *screen) scrollDown(n int, blank Cell) {
	if n > 0 {
		s.shiftDown(s.scroll.top, n, blank)
	}
}

func (s *screen) removeLines(y, n int, blank Cell) {
	n = min(n, s.scroll.bottom-y)
	if s.history != nil && y == s.scroll.top && s.fullWidth() {
		for i := y; i < y+n; i++ {
			s.history.push(s.rows[i])
		}
	}
	if s.fullWidth() {
		lines := s.rows[y:s.scroll.bottom]
		rotateLeft(lines, n)
		s.blankLines(lines[len(lines)-n:], blank)
		return
	}
	s.splitRegion(y, s.scroll.bottom)
	for i := y; i < s.scroll.bottom-n; i++ {
		copy(s.rows[i].cells[s.scroll.left:s.scroll.right], s.rows[i+n].cells[s.scroll.left:s.scroll.right])
	}
	s.fillArea(s.scroll.left, s.scroll.bottom-n, s.scroll.right, s.scroll.bottom, blank)
}

func (s *screen) shiftDown(y, n int, blank Cell) {
	n = min(n, s.scroll.bottom-y)
	if s.fullWidth() {
		lines := s.rows[y:s.scroll.bottom]
		rotateLeft(lines, len(lines)-n)
		s.blankLines(lines[:n], blank)
		return
	}
	s.splitRegion(y, s.scroll.bottom)
	for i := s.scroll.bottom - 1; i >= y+n; i-- {
		copy(s.rows[i].cells[s.scroll.left:s.scroll.right], s.rows[i-n].cells[s.scroll.left:s.scroll.right])
	}
	s.fillArea(s.scroll.left, y, s.scroll.right, y+n, blank)
}

func (s *screen) splitRegion(top, bottom int) {
	for y := top; y < bottom; y++ {
		s.rows[y].splitAt(s.scroll.left)
		s.rows[y].splitAt(s.scroll.right)
		s.rows[y].wrapped = false
	}
}

func (s *screen) blankLines(lines []row, blank Cell) {
	for i := range lines {
		lines[i].fill(0, len(lines[i].cells), blank)
		lines[i].wrapped = false
	}
}

func (s *screen) resize(columns, rows int) {
	for y := range s.rows {
		s.rows[y].resize(columns)
	}
	if rows < len(s.rows) {
		clear(s.rows[rows:])
		s.rows = slices.Clip(s.rows[:rows])
	}
	for len(s.rows) < rows {
		s.rows = append(s.rows, newRow(columns, blankCell(Color{})))
	}
	s.scroll = s.bounds()
	s.cursor = s.resizedCursor(s.cursor)
	s.saved.cursor = s.resizedCursor(s.saved.cursor)
}

func (s *screen) resizedCursor(c cursor) cursor {
	x := c.x
	if c.wrapPending && x < s.columns()-1 {
		x++
	}
	return cursor{x: clamp(x, 0, s.columns()-1), y: clamp(c.y, 0, s.height()-1)}
}

func rotateLeft(lines []row, n int) {
	slices.Reverse(lines[:n])
	slices.Reverse(lines[n:])
	slices.Reverse(lines)
}

func clamp(v, lo, hi int) int {
	return max(lo, min(v, hi))
}
