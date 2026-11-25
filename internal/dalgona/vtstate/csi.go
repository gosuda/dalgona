package vtstate

import "github.com/charmbracelet/x/ansi"

type csiHandler func(*Terminal, ansi.Params)

var csiHandlers = map[int]csiHandler{
	'@':                         (*Terminal).insertCharacters,
	'A':                         func(t *Terminal, p ansi.Params) { t.screen().moveCursor(0, -count(p)) },
	'B':                         func(t *Terminal, p ansi.Params) { t.screen().moveCursor(0, count(p)) },
	'C':                         func(t *Terminal, p ansi.Params) { t.screen().moveCursor(count(p), 0) },
	'D':                         func(t *Terminal, p ansi.Params) { t.screen().moveCursor(-count(p), 0) },
	'E':                         (*Terminal).cursorNextLine,
	'F':                         (*Terminal).cursorPreviousLine,
	'G':                         (*Terminal).cursorColumn,
	'H':                         (*Terminal).cursorPosition,
	'I':                         func(t *Terminal, p ansi.Params) { t.nextTab(count(p)) },
	'J':                         (*Terminal).eraseDisplay,
	'K':                         (*Terminal).eraseLine,
	'L':                         (*Terminal).insertLines,
	'M':                         (*Terminal).deleteLines,
	'P':                         (*Terminal).deleteCharacters,
	'S':                         func(t *Terminal, p ansi.Params) { t.screen().scrollUp(count(p), t.blank()) },
	'T':                         func(t *Terminal, p ansi.Params) { t.screen().scrollDown(count(p), t.blank()) },
	ansi.Command('?', 0, 'W'):   (*Terminal).resetTabStops,
	'X':                         func(t *Terminal, p ansi.Params) { t.eraseCharacters(count(p)) },
	'Z':                         func(t *Terminal, p ansi.Params) { t.previousTab(count(p)) },
	'`':                         (*Terminal).cursorColumn,
	'a':                         (*Terminal).cursorColumnRelative,
	'b':                         (*Terminal).repeatCharacter,
	'c':                         (*Terminal).primaryAttributes,
	ansi.Command('>', 0, 'c'):   (*Terminal).secondaryAttributes,
	'd':                         (*Terminal).cursorRowAbsolute,
	'e':                         (*Terminal).cursorRowRelative,
	'f':                         (*Terminal).cursorPosition,
	'g':                         (*Terminal).clearTabStops,
	'h':                         func(t *Terminal, p ansi.Params) { t.handleMode(p, true, false) },
	ansi.Command('?', 0, 'h'):   func(t *Terminal, p ansi.Params) { t.handleMode(p, true, true) },
	'l':                         func(t *Terminal, p ansi.Params) { t.handleMode(p, false, false) },
	ansi.Command('?', 0, 'l'):   func(t *Terminal, p ansi.Params) { t.handleMode(p, false, true) },
	'm':                         func(t *Terminal, p ansi.Params) { readStyle(p, &t.style) },
	'n':                         (*Terminal).statusReport,
	ansi.Command('?', 0, 'n'):   (*Terminal).extendedStatusReport,
	ansi.Command(0, '$', 'p'):   func(t *Terminal, p ansi.Params) { t.requestMode(p, false) },
	ansi.Command('?', '$', 'p'): func(t *Terminal, p ansi.Params) { t.requestMode(p, true) },
	ansi.Command(0, ' ', 'q'):   (*Terminal).setCursorStyle,
	'r':                         (*Terminal).setTopBottomMargins,
	's':                         (*Terminal).setLeftRightMarginsOrSave,
}

func (t *Terminal) handleCsi(cmd ansi.Cmd, params ansi.Params) {
	t.commitCluster()
	if handle, ok := csiHandlers[int(cmd)]; ok {
		handle(t, params)
	}
}

func count(params ansi.Params) int {
	n, _, _ := params.Param(0, 1)
	return max(n, 1)
}

func (t *Terminal) insertCharacters(params ansi.Params) {
	s := t.screen()
	if s.cursor.x < s.scroll.left || s.cursor.x >= s.scroll.right {
		return
	}
	s.rows[s.cursor.y].insertBlanks(s.cursor.x, count(params), s.scroll.right, t.blank())
}

func (t *Terminal) deleteCharacters(params ansi.Params) {
	s := t.screen()
	if s.cursor.x < s.scroll.left || s.cursor.x >= s.scroll.right {
		return
	}
	s.rows[s.cursor.y].deleteCells(s.cursor.x, count(params), s.scroll.right, t.blank())
}

func (t *Terminal) cursorNextLine(params ansi.Params) {
	t.screen().moveCursor(0, count(params))
	t.carriageReturn()
}

func (t *Terminal) cursorPreviousLine(params ansi.Params) {
	t.screen().moveCursor(0, -count(params))
	t.carriageReturn()
}

func (t *Terminal) cursorColumn(params ansi.Params) {
	n, _, _ := params.Param(0, 1)
	s := t.screen()
	s.setCursor(n-1, s.cursor.y, false)
}

func (t *Terminal) cursorPosition(params ansi.Params) {
	row, _, _ := params.Param(0, 1)
	column, _, _ := params.Param(1, 1)
	t.goTo(max(column, 1)-1, max(row, 1)-1)
}

func (t *Terminal) cursorColumnRelative(params ansi.Params) {
	s := t.screen()
	s.setCursor(s.cursor.x+count(params), s.cursor.y, false)
}

func (t *Terminal) cursorRowAbsolute(params ansi.Params) {
	n, _, _ := params.Param(0, 1)
	s := t.screen()
	if t.modes.has(modeOrigin) {
		s.setCursor(s.cursor.x-s.scroll.left, n-1, true)
		return
	}
	s.setCursor(s.cursor.x, n-1, false)
}

func (t *Terminal) cursorRowRelative(params ansi.Params) {
	s := t.screen()
	s.setCursor(s.cursor.x, s.cursor.y+count(params), false)
}

func (t *Terminal) eraseDisplay(params ansi.Params) {
	n, _, _ := params.Param(0, 0)
	s := t.screen()
	columns, rows := s.columns(), s.height()
	x, y := s.cursor.x, s.cursor.y
	switch n {
	case 0:
		s.fillArea(x, y, columns, y+1, t.blank())
		s.fillArea(0, y+1, columns, rows, t.blank())
	case 1:
		s.fillArea(0, 0, columns, y, t.blank())
		s.fillArea(0, y, x+1, y+1, t.blank())
	case 2:
		t.clearToHistory()
	case 3:
		s.clear()
		if s.history != nil {
			s.history.clear()
		}
	}
}

func (t *Terminal) clearToHistory() {
	s := t.screen()
	if s.history != nil {
		for _, r := range s.rows {
			if !r.blank() {
				s.history.push(r)
			}
		}
	}
	s.clear()
}

func (t *Terminal) eraseLine(params ansi.Params) {
	n, _, _ := params.Param(0, 0)
	s := t.screen()
	x, y := s.cursor.x, s.cursor.y
	switch n {
	case 0:
		t.eraseCharacters(s.columns() - x)
	case 1:
		s.fillArea(0, y, x+1, y+1, t.blank())
	case 2:
		s.fillArea(0, y, s.columns(), y+1, t.blank())
	}
}

func (t *Terminal) insertLines(params ansi.Params) {
	s := t.screen()
	if s.insertLines(count(params), t.blank()) {
		s.cursor = cursor{x: s.scroll.left, y: s.cursor.y}
	}
}

func (t *Terminal) deleteLines(params ansi.Params) {
	s := t.screen()
	if s.deleteLines(count(params), t.blank()) {
		s.cursor = cursor{x: s.scroll.left, y: s.cursor.y}
	}
}

func (t *Terminal) resetTabStops(params ansi.Params) {
	if len(params) == 1 && params[0].Param(0) == 5 {
		t.tabs = defaultTabs(len(t.tabs))
	}
}

func (t *Terminal) clearTabStops(params ansi.Params) {
	n, _, _ := params.Param(0, 0)
	switch n {
	case 0:
		t.tabs[t.screen().cursor.x] = false
	case 3:
		clear(t.tabs)
	}
}

func (t *Terminal) repeatCharacter(params ansi.Params) {
	if t.lastChar == 0 {
		return
	}
	columns, rows := t.Size()
	for range min(count(params), columns*rows) {
		t.print(t.lastChar)
	}
}

func (t *Terminal) primaryAttributes(params ansi.Params) {
	if n, _, _ := params.Param(0, 0); n == 0 {
		t.reply(ansi.PrimaryDeviceAttributes(62, 1, 6, 22))
	}
}

func (t *Terminal) secondaryAttributes(params ansi.Params) {
	if n, _, _ := params.Param(0, 0); n == 0 {
		t.reply(ansi.SecondaryDeviceAttributes(1, 10, 0))
	}
}

func (t *Terminal) statusReport(params ansi.Params) {
	n, _, ok := params.Param(0, 1)
	if !ok {
		return
	}
	c := t.screen().cursor
	switch n {
	case 5:
		t.reply(ansi.DeviceStatusReport(ansi.DECStatusReport(0)))
	case 6:
		t.reply(ansi.CursorPositionReport(c.y+1, c.x+1))
	}
}

func (t *Terminal) extendedStatusReport(params ansi.Params) {
	n, _, ok := params.Param(0, 1)
	if ok && n == 6 {
		c := t.screen().cursor
		t.reply(ansi.ExtendedCursorPositionReport(c.y+1, c.x+1, 0))
	}
}

func (t *Terminal) setCursorStyle(params ansi.Params) {
	n, _, _ := params.Param(0, 0)
	n = max(n, 1)
	if n > 6 {
		return
	}
	t.blink = n%2 == 1
	t.shape = CursorShape((n - 1) / 2)
}

func (t *Terminal) setTopBottomMargins(params ansi.Params) {
	s := t.screen()
	top, _, _ := params.Param(0, 1)
	bottom, _, _ := params.Param(1, s.height())
	top = max(top, 1)
	if bottom < 1 {
		bottom = s.height()
	}
	bottom = min(bottom, s.height())
	if top >= bottom {
		return
	}
	s.scroll.top, s.scroll.bottom = top-1, bottom
	t.goTo(0, 0)
}

func (t *Terminal) setLeftRightMarginsOrSave(params ansi.Params) {
	if !t.modes.has(modeLeftRight) {
		t.saveCursor()
		return
	}
	s := t.screen()
	left, _, _ := params.Param(0, 1)
	right, _, _ := params.Param(1, s.columns())
	left = max(left, 1)
	if right < 1 {
		right = s.columns()
	}
	right = min(right, s.columns())
	if left >= right {
		return
	}
	s.scroll.left, s.scroll.right = left-1, right
	t.goTo(0, 0)
}
