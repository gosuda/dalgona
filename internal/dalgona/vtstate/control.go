package vtstate

import "github.com/charmbracelet/x/ansi"

var controlHandlers = map[byte]func(*Terminal){
	ansi.BS:  (*Terminal).backspace,
	ansi.HT:  func(t *Terminal) { t.nextTab(1) },
	ansi.LF:  (*Terminal).linefeed,
	ansi.VT:  (*Terminal).linefeed,
	ansi.FF:  (*Terminal).linefeed,
	ansi.CR:  (*Terminal).carriageReturn,
	ansi.SO:  func(t *Terminal) { t.charsets.gl = 1 },
	ansi.SI:  func(t *Terminal) { t.charsets.gl = 0 },
	ansi.IND: (*Terminal).index,
	ansi.HTS: (*Terminal).horizontalTabSet,
	ansi.RI:  (*Terminal).reverseIndex,
	ansi.SS2: func(t *Terminal) { t.charsets.single = 2 },
	ansi.SS3: func(t *Terminal) { t.charsets.single = 3 },
}

func (t *Terminal) execute(b byte) {
	if t.replaying {
		return
	}
	t.commitCluster()
	if handle, ok := controlHandlers[b]; ok {
		handle(t)
	}
}

func (t *Terminal) index() {
	s := t.screen()
	x, y := s.cursor.x, s.cursor.y
	switch {
	case y == s.scroll.bottom-1 && x >= s.scroll.left && x < s.scroll.right:
		s.scrollUp(1, t.blank())
		s.cursor.wrapPending = false
	case y < s.scroll.bottom-1 || !s.scroll.contains(x, y):
		s.moveCursor(0, 1)
	default:
		s.cursor.wrapPending = false
	}
}

func (t *Terminal) linefeed() {
	t.index()
	if t.modes.has(modeLNM) {
		t.carriageReturn()
	}
}

func (t *Terminal) reverseIndex() {
	s := t.screen()
	x, y := s.cursor.x, s.cursor.y
	if y == s.scroll.top && x >= s.scroll.left && x < s.scroll.right {
		s.scrollDown(1, t.blank())
		s.cursor.wrapPending = false
		return
	}
	s.moveCursor(0, -1)
}

func (t *Terminal) carriageReturn() {
	s := t.screen()
	x := 0
	if t.modes.has(modeOrigin) || s.scroll.contains(s.cursor.x, s.cursor.y) {
		x = s.scroll.left
	}
	s.cursor = cursor{x: x, y: s.cursor.y}
}

func (t *Terminal) backspace() {
	t.screen().moveCursor(-1, 0)
}

func (t *Terminal) horizontalTabSet() {
	t.tabs[t.screen().cursor.x] = true
}

func (t *Terminal) nextTab(n int) {
	s := t.screen()
	x := s.cursor.x
	for range n {
		next := t.nextTabStop(x)
		if next <= x {
			break
		}
		x = next
	}
	if x >= s.scroll.right {
		x = min(s.scroll.right, s.columns()) - 1
	}
	s.cursor = cursor{x: x, y: s.cursor.y}
}

func (t *Terminal) nextTabStop(x int) int {
	last := len(t.tabs) - 1
	for x < last {
		x++
		if t.tabs[x] {
			return x
		}
	}
	return x
}

func (t *Terminal) previousTab(n int) {
	s := t.screen()
	x := s.cursor.x
	for range n {
		previous := t.previousTabStop(x)
		if previous >= x {
			break
		}
		x = previous
	}
	if t.modes.has(modeOrigin) {
		x = max(x, s.scroll.left)
	}
	s.cursor = cursor{x: x, y: s.cursor.y}
}

func (t *Terminal) previousTabStop(x int) int {
	for x > 0 {
		x--
		if t.tabs[x] {
			return x
		}
	}
	return x
}

func (t *Terminal) saveCursor() {
	s := t.screen()
	s.saved = savedCursor{
		cursor:   s.cursor,
		style:    t.style,
		link:     t.link,
		origin:   t.modes.has(modeOrigin),
		charsets: t.charsets,
	}
}

func (t *Terminal) restoreCursor() {
	s := t.screen()
	s.cursor = s.saved.cursor
	t.style = s.saved.style
	t.link = s.saved.link
	t.charsets = s.saved.charsets
	if s.saved.origin {
		t.modes |= modeOrigin
	} else {
		t.modes &^= modeOrigin
	}
}

func (t *Terminal) eraseCharacters(n int) {
	s := t.screen()
	s.fillArea(s.cursor.x, s.cursor.y, s.cursor.x+max(n, 1), s.cursor.y+1, t.blank())
	s.cursor.wrapPending = false
}

func (t *Terminal) goTo(x, y int) {
	t.screen().setCursor(x, y, t.modes.has(modeOrigin))
}
