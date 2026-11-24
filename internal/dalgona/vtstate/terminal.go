package vtstate

import (
	"fmt"
	"unicode/utf8"

	"github.com/charmbracelet/x/ansi"
	"github.com/charmbracelet/x/ansi/parser"
)

const (
	MaxColumns           = 240
	MaxRows              = 120
	maxClusterBytes      = 64
	parserDataBytes      = 8 << 10
	maxContinuationBytes = 64 << 10
)

type CursorShape uint8

const (
	CursorBlock CursorShape = iota
	CursorUnderline
	CursorBar
)

type CursorState struct {
	Column      int
	Row         int
	WrapPending bool
	Visible     bool
	Shape       CursorShape
	Blink       bool
}

type Terminal struct {
	parser            *ansi.Parser
	link              *Link
	tabs              []bool
	cluster           []byte
	replies           []byte
	pending           sequenceTracker
	history           scrollback
	screens           [2]screen
	active            int
	lastChar          rune
	style             Style
	charsets          charsets
	modes             modeSet
	shape             CursorShape
	blink             bool
	recordReplies     bool
	droppedScrollback int
	replaying         bool
}

func New(columns, rows int) *Terminal {
	t := newTerminal(columns, rows)
	t.recordReplies = true
	return t
}

func newTerminal(columns, rows int) *Terminal {
	checkSize(columns, rows)
	t := &Terminal{}
	t.screens[0] = newScreen(columns, rows, &t.history)
	t.screens[1] = newScreen(columns, rows, nil)
	t.reset()
	t.parser = new(ansi.Parser)
	t.parser.SetParamsSize(parser.MaxParamsSize)
	t.parser.SetDataSize(parserDataBytes)
	t.parser.SetHandler(ansi.Handler{
		Print:     t.print,
		Execute:   t.execute,
		HandleCsi: t.handleCsi,
		HandleEsc: t.handleEsc,
		HandleDcs: t.ignoreDcs,
		HandleOsc: t.handleOsc,
		HandleApc: t.ignoreString,
		HandlePm:  t.ignoreString,
		HandleSos: t.ignoreString,
	})
	return t
}

func checkSize(columns, rows int) {
	if columns < 1 || columns > MaxColumns || rows < 1 || rows > MaxRows {
		panic(fmt.Sprintf("vtstate: terminal size %dx%d is outside 1..%d columns and 1..%d rows", columns, rows, MaxColumns, MaxRows))
	}
}

func (t *Terminal) reset() {
	columns := t.screens[0].columns()
	t.screens[0].reset()
	t.screens[1].reset()
	t.active = 0
	t.tabs = defaultTabs(columns)
	t.modes = defaultModes
	t.charsets = defaultCharsets()
	t.style = Style{}
	t.link = nil
	t.shape = CursorBlock
	t.blink = true
	t.lastChar = 0
	t.cluster = t.cluster[:0]
}

func defaultTabs(columns int) []bool {
	tabs := make([]bool, columns)
	for x := 0; x < columns; x += 8 {
		tabs[x] = true
	}
	return tabs
}

func (t *Terminal) Write(p []byte) (int, error) {
	t.Feed(p)
	return len(p), nil
}

func (t *Terminal) Feed(p []byte) {
	for _, b := range p {
		before := t.parser.State()
		action := t.parser.Advance(b)
		t.pending.track(b, before, action, t.parser.State())
	}
}

func (t *Terminal) Resize(columns, rows int) {
	checkSize(columns, rows)
	for i := range t.screens {
		t.screens[i].resize(columns, rows)
	}
	t.history.truncate(columns)
	t.tabs = defaultTabs(columns)
	if t.modes.has(modeInBandResize) {
		t.replyResize()
	}
}

func (t *Terminal) TakeReplies() []byte {
	replies := t.replies
	t.replies = nil
	return replies
}

func (t *Terminal) reply(s string) {
	if t.recordReplies {
		t.replies = append(t.replies, s...)
	}
}

func (t *Terminal) replyResize() {
	s := t.screen()
	t.reply(ansi.InBandResize(s.height(), s.columns(), 0, 0))
}

func (t *Terminal) screen() *screen {
	return &t.screens[t.active]
}

func (t *Terminal) blank() Cell {
	return blankCell(t.style.Background)
}

func (t *Terminal) Size() (columns, rows int) {
	s := t.screen()
	return s.columns(), s.height()
}

func (t *Terminal) AlternateScreen() bool {
	return t.active == 1
}

func (t *Terminal) AppendRow(dst []Cell, y int) []Cell {
	start := len(dst)
	dst = append(dst, t.screen().rows[y].cells...)
	if pending, ok := t.PendingCell(); ok && pending.Row == y {
		row{cells: dst[start:]}.set(pending.Column, pending.Cell)
	}
	return dst
}

func (t *Terminal) RowWrapped(y int) bool {
	return t.screen().rows[y].wrapped
}

func (t *Terminal) ScrollbackLen() int {
	return t.history.len()
}

func (t *Terminal) AppendScrollback(dst []Cell, i int) []Cell {
	return append(dst, t.history.row(i).cells...)
}

func (t *Terminal) ScrollbackWrapped(i int) bool {
	return t.history.row(i).wrapped
}

func (t *Terminal) Cursor() CursorState {
	c := t.screen().cursor
	return CursorState{
		Column:      c.x,
		Row:         c.y,
		WrapPending: c.wrapPending,
		Visible:     t.modes.has(modeCursorVisible),
		Shape:       t.shape,
		Blink:       t.blink,
	}
}

type PlacedCell struct {
	Cell   Cell
	Column int
	Row    int
}

func (t *Terminal) PendingCell() (PlacedCell, bool) {
	if len(t.cluster) == 0 {
		return PlacedCell{}, false
	}
	cluster, width := ansi.FirstGraphemeCluster(t.cluster, ansi.GraphemeWidth)
	if width <= 0 {
		return PlacedCell{}, false
	}
	c := t.cellFor(string(cluster), width)
	x, y, ok := t.screen().provisional(int(c.Width), t.modes.has(modeAutoWrap))
	return PlacedCell{Cell: c, Column: x, Row: y}, ok
}

func (s *screen) provisional(width int, autoWrap bool) (x, y int, ok bool) {
	x, y = s.cursor.x, s.cursor.y
	if !autoWrap || (!s.cursor.wrapPending && x+width <= s.columns()) {
		return x, y, true
	}
	if y == s.scroll.bottom-1 && x >= s.scroll.left && x < s.scroll.right {
		return 0, y, true
	}
	if y+1 >= s.height() {
		return 0, y, true
	}
	return 0, y + 1, true
}

func (t *Terminal) ScrollbackPushed() uint64 {
	return t.history.pushed
}

func (t *Terminal) DroppedScrollbackRows() int {
	return t.droppedScrollback
}

func (t *Terminal) print(r rune) {
	if r < utf8.RuneSelf {
		t.commitCluster()
		if r < 0x20 || r == 0x7f {
			return
		}
		t.cluster = append(t.cluster, byte(r)) //nolint:gosec // r is ASCII below RuneSelf per the guard above
		return
	}
	if r < 0xa0 {
		t.commitCluster()
		return
	}
	if len(t.cluster)+utf8.RuneLen(r) > maxClusterBytes {
		t.commitCluster()
	}
	t.cluster = utf8.AppendRune(t.cluster, r)
	first, width := ansi.FirstGraphemeCluster(t.cluster, ansi.GraphemeWidth)
	if len(first) == len(t.cluster) {
		return
	}
	t.writeGrapheme(string(first), width)
	t.cluster = t.cluster[:copy(t.cluster, t.cluster[len(first):])]
}

func (t *Terminal) commitCluster() {
	if len(t.cluster) == 1 {
		t.writeGrapheme(string(t.cluster), 1)
		t.cluster = t.cluster[:0]
		return
	}
	rest := t.cluster
	for len(rest) > 0 {
		first, width := ansi.FirstGraphemeCluster(rest, ansi.GraphemeWidth)
		if len(first) == 0 {
			break
		}
		t.writeGrapheme(string(first), width)
		rest = rest[len(first):]
	}
	t.cluster = t.cluster[:0]
}

func (t *Terminal) writeGrapheme(content string, width int) {
	if width <= 0 {
		return
	}
	c := t.cellFor(content, width)
	t.charsets.single = 0
	if c.Width == 1 && len(content) == 1 {
		t.lastChar = rune(content[0])
	}
	t.place(c)
}

func (t *Terminal) cellFor(content string, width int) Cell {
	text, mapped := t.charsets.mapped(content, t.charsets.single)
	if mapped {
		width = 1
	}
	return Cell{Text: text, Width: uint8(min(width, 2)), Style: t.style, Link: t.link}
}

func (t *Terminal) place(c Cell) {
	s := t.screen()
	columns, width := s.columns(), int(c.Width)
	autoWrap := t.modes.has(modeAutoWrap)
	if autoWrap && (s.cursor.wrapPending || s.cursor.x+width > columns) {
		s.rows[s.cursor.y].wrapped = true
		t.index()
		s.cursor = cursor{y: s.cursor.y}
	}
	x, y := s.cursor.x, s.cursor.y
	s.rows[y].set(x, c)
	if autoWrap && x+width >= columns {
		s.cursor = cursor{x: columns - 1, y: y, wrapPending: true}
		return
	}
	s.cursor = cursor{x: min(x+width, columns-1), y: y}
}

func (t *Terminal) ignoreDcs(ansi.Cmd, ansi.Params, []byte) {
	t.commitCluster()
}

func (t *Terminal) ignoreString([]byte) {
	t.commitCluster()
}
