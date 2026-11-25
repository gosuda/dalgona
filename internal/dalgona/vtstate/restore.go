package vtstate

import (
	"bytes"
	"encoding/base64"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
)

const maxParserStateBase64 = (maxContinuationBytes + 2) / 3 * 4

type bodyReader struct {
	dec     *jsontext.Decoder
	err     error
	pending string
	peeked  bool
}

func (r *bodyReader) fail(err error) {
	if r.err == nil {
		r.err = err
	}
}

func (r *bodyReader) failf(format string, args ...any) {
	r.fail(invalid(format+" at %s", append(args, r.dec.StackPointer())...))
}

func (r *bodyReader) next() jsontext.Token {
	if r.err != nil {
		return jsontext.Token{}
	}
	tok, err := r.dec.ReadToken()
	if err != nil {
		r.fail(invalid("%v", err))
		return jsontext.Token{}
	}
	return tok
}

func (r *bodyReader) expect(kind jsontext.Kind) {
	tok := r.next()
	if r.err == nil && tok.Kind() != kind {
		r.failf("found %v, want %v", tok.Kind(), kind)
	}
}

func (r *bodyReader) more() bool {
	if r.err != nil {
		return false
	}
	if r.dec.PeekKind() == jsontext.KindEndArray {
		r.next()
		return false
	}
	return true
}

func (r *bodyReader) takeName() (string, bool) {
	if r.peeked {
		r.peeked = false
		return r.pending, true
	}
	if r.err != nil || r.dec.PeekKind() != jsontext.KindString {
		return "", false
	}
	return r.next().String(), r.err == nil
}

func (r *bodyReader) name(want string) {
	got, ok := r.takeName()
	switch {
	case r.err != nil:
	case !ok:
		r.failf("missing member %q", want)
	case got != want:
		r.failf("member %q, want %q", got, want)
	}
}

func (r *bodyReader) optional(want string) bool {
	got, ok := r.takeName()
	if !ok {
		return false
	}
	if got == want {
		return true
	}
	r.pending, r.peeked = got, true
	return false
}

func (r *bodyReader) endObject() {
	if r.peeked {
		r.failf("unknown member %q", r.pending)
		return
	}
	r.expect(jsontext.KindEndObject)
}

func (r *bodyReader) uint(limit int) int {
	tok := r.next()
	if r.err != nil {
		return 0
	}
	if limit < 0 {
		r.failf("limit %d is negative", limit)
		return 0
	}
	if tok.Kind() != jsontext.KindNumber {
		r.failf("found %v, want a number", tok.Kind())
		return 0
	}
	v, err := tok.Uint()
	if err != nil || v > uint64(limit) { //nolint:gosec // limit is checked non-negative above
		r.failf("number %s is outside 0 through %d", tok.String(), limit)
		return 0
	}
	return int(v) //nolint:gosec // v does not exceed limit, and every limit fits in int
}

func (r *bodyReader) string(limit int) string {
	tok := r.next()
	if r.err != nil {
		return ""
	}
	if tok.Kind() != jsontext.KindString || len(tok.String()) > limit {
		r.failf("want a string of at most %d bytes", limit)
		return ""
	}
	return tok.String()
}

func (r *bodyReader) boolean() bool {
	tok := r.next()
	switch {
	case r.err != nil:
		return false
	case tok.Kind() == jsontext.KindTrue:
		return true
	case tok.Kind() != jsontext.KindFalse:
		r.failf("found %v, want a boolean", tok.Kind())
	}
	return false
}

func (r *bodyReader) enum(names []string) int {
	s := r.string(32)
	for i, n := range names {
		if s == n {
			return i
		}
	}
	if r.err == nil {
		r.failf("unknown value %q", s)
	}
	return 0
}

func (r *bodyReader) cappedUint(limit int) uint8 {
	v := r.uint(limit)
	if v < 0 || v > 255 {
		r.failf("number %d is outside 0 through 255", v)
		return 0
	}
	return uint8(v) //nolint:gosec // v is checked to fit in a byte above
}

func (r *bodyReader) cappedEnum(names []string) uint8 {
	v := r.enum(names)
	if v < 0 || v > 255 {
		r.failf("value %d is outside 0 through 255", v)
		return 0
	}
	return uint8(v) //nolint:gosec // v is checked to fit in a byte above
}

type checkpointReader struct {
	bodyReader
	styles  []Style
	links   []*Link
	columns int
	rows    int
}

type decodedScreen struct {
	rows   []row
	saved  savedCursor
	cursor cursor
	scroll region
}

func Restore(body []byte) (*Terminal, error) {
	if len(body) > MaxCheckpointBytes {
		return nil, fmt.Errorf("%w: checkpoint has %d bytes, more than %d", ErrLimitExceeded, len(body), MaxCheckpointBytes)
	}
	r := &checkpointReader{bodyReader: bodyReader{dec: jsontext.NewDecoder(bytes.NewReader(body), jsontext.AllowDuplicateNames(true))}}
	t, err := r.terminal()
	if err != nil {
		return nil, err
	}
	canonical, err := t.Checkpoint()
	if err != nil {
		return nil, err
	}
	if !bytes.Equal(canonical, body) {
		return nil, invalid("checkpoint is not in canonical VTStateV1 form")
	}
	return t, nil
}

func (r *checkpointReader) terminal() (*Terminal, error) {
	if err := r.header(); err != nil {
		return nil, err
	}
	r.name("styles")
	r.readStyles()
	r.name("links")
	r.readLinks()
	r.name("currentStyle")
	style := r.style()
	var link *Link
	if r.optional("currentLink") {
		link = r.link()
	}
	r.name("activeScreen")
	active := r.enum(screenNames)
	r.name("primary")
	primary := r.screen()
	r.name("alternate")
	alternate := r.screen()
	r.name("scrollback")
	history := r.scrollback()
	r.name("droppedScrollbackRows")
	dropped := r.uint(1 << 31)
	t := r.settings()
	if r.err != nil {
		return nil, r.err
	}
	t.screens[0].rows, t.screens[0].cursor, t.screens[0].saved, t.screens[0].scroll = primary.rows, primary.cursor, primary.saved, primary.scroll
	t.screens[1].rows, t.screens[1].cursor, t.screens[1].saved, t.screens[1].scroll = alternate.rows, alternate.cursor, alternate.saved, alternate.scroll
	t.history = history
	t.droppedScrollback = dropped
	t.active = active
	t.style = style
	t.link = link
	return t, r.continuation(t)
}

func (r *checkpointReader) header() error {
	r.expect(jsontext.KindBeginObject)
	r.name("version")
	version := r.uint(1 << 31)
	if r.err != nil {
		return r.err
	}
	if version != Version {
		return fmt.Errorf("%w: checkpoint version %d, this build reads version %d; attach with a compatible backend", ErrIncompatible, version, Version)
	}
	r.name("engineVersion")
	engine := r.string(512)
	if r.err != nil {
		return r.err
	}
	if engine != EngineVersion() {
		return fmt.Errorf("%w: checkpoint engine %q, this build runs %q; attach with a compatible backend", ErrIncompatible, engine, EngineVersion())
	}
	return nil
}

func (r *checkpointReader) readStyles() {
	r.expect(jsontext.KindBeginArray)
	for r.more() {
		var s Style
		r.expect(jsontext.KindBeginObject)
		r.name("foreground")
		s.Foreground = r.color()
		r.name("background")
		s.Background = r.color()
		r.name("underlineColor")
		s.UnderlineColor = r.color()
		r.name("underline")
		s.Underline = Underline(r.cappedEnum(underlineNames))
		r.name("attributes")
		s.Attributes = r.attributes()
		r.endObject()
		r.styles = append(r.styles, s)
	}
}

func (r *checkpointReader) attributes() Attributes {
	var attrs Attributes
	last := -1
	r.expect(jsontext.KindBeginArray)
	for r.more() {
		i := r.enum(attributeNames)
		if i <= last {
			r.failf("attributes are not in canonical order")
		}
		last = i
		attrs |= 1 << i
	}
	return attrs
}

func (r *checkpointReader) color() Color {
	var c Color
	r.expect(jsontext.KindBeginObject)
	r.name("kind")
	c.Kind = ColorKind(r.cappedEnum(colorKindNames))
	switch c.Kind {
	case ColorDefault:
	case ColorIndexed:
		r.name("index")
		c.Index = r.cappedUint(255)
	case ColorRGB:
		r.name("red")
		c.Red = r.cappedUint(255)
		r.name("green")
		c.Green = r.cappedUint(255)
		r.name("blue")
		c.Blue = r.cappedUint(255)
	}
	r.endObject()
	return c
}

func (r *checkpointReader) readLinks() {
	r.expect(jsontext.KindBeginArray)
	for r.more() {
		var l Link
		r.expect(jsontext.KindBeginObject)
		r.name("uri")
		l.URI = r.string(MaxLinkURIBytes)
		r.name("id")
		l.ID = r.string(MaxLinkIDBytes)
		r.endObject()
		if r.err == nil && !l.allowed() {
			r.failf("link %q is outside the hyperlink allowlist", l.URI)
		}
		r.links = append(r.links, &l)
	}
}

func (r *checkpointReader) style() Style {
	if len(r.styles) == 0 {
		r.failf("style index without styles")
		return Style{}
	}
	return r.styles[r.uint(len(r.styles)-1)]
}

func (r *checkpointReader) link() *Link {
	if len(r.links) == 0 {
		r.failf("link index without links")
		return nil
	}
	return r.links[r.uint(len(r.links)-1)]
}

func (r *checkpointReader) screen() decodedScreen {
	var s decodedScreen
	r.expect(jsontext.KindBeginObject)
	r.name("rows")
	r.expect(jsontext.KindBeginArray)
	for r.more() {
		if len(s.rows) == MaxRows {
			r.failf("screen has more than %d rows", MaxRows)
		}
		s.rows = append(s.rows, r.row())
	}
	switch {
	case r.err != nil:
		return s
	case len(s.rows) == 0:
		r.failf("screen has no rows")
		return s
	case r.rows == 0:
		r.rows = len(s.rows)
	case len(s.rows) != r.rows:
		r.failf("screen has %d rows, want %d", len(s.rows), r.rows)
		return s
	}
	r.name("cursor")
	s.cursor = r.cursor()
	r.name("savedCursor")
	s.saved = r.savedCursor()
	r.name("margins")
	s.scroll = r.margins()
	r.endObject()
	return s
}

func (r *checkpointReader) row() row {
	var out row
	r.expect(jsontext.KindBeginObject)
	r.name("cells")
	r.expect(jsontext.KindBeginArray)
	for r.more() {
		if len(out.cells) == MaxColumns {
			r.failf("row has more than %d cells", MaxColumns)
		}
		out.cells = append(out.cells, r.cell())
	}
	r.name("softWrapped")
	out.wrapped = r.boolean()
	r.endObject()
	if r.err != nil {
		return out
	}
	if r.columns == 0 {
		r.columns = len(out.cells)
	}
	if len(out.cells) != r.columns || r.columns == 0 {
		r.failf("row has %d cells, want %d", len(out.cells), r.columns)
		return out
	}
	if err := checkGeometry(out.cells); err != nil {
		r.fail(err)
	}
	return out
}

func (r *checkpointReader) cell() Cell {
	var c Cell
	r.expect(jsontext.KindBeginObject)
	r.name("text")
	c.Text = r.string(maxClusterBytes)
	r.name("width")
	c.Width = r.cappedUint(2)
	r.name("style")
	c.Style = r.style()
	if r.optional("link") {
		c.Link = r.link()
	}
	r.endObject()
	return c
}

func checkGeometry(cells []Cell) error {
	for x := range cells {
		if err := checkCellGeometry(cells, x); err != nil {
			return err
		}
	}
	return nil
}

func checkCellGeometry(cells []Cell, x int) error {
	c := cells[x]
	if c.Width == 0 {
		return checkContinuationGeometry(cells, x, c)
	}
	if !singleCluster(c.Text) {
		return invalid("cell %d text is not one printable grapheme cluster", x)
	}
	if c.Width == 2 && (x+1 == len(cells) || cells[x+1].Width != 0) {
		return invalid("wide cell %d has no continuation", x)
	}
	return nil
}

func checkContinuationGeometry(cells []Cell, x int, c Cell) error {
	if x > 0 && cells[x-1].Width == 2 && c.Equal(Cell{}) {
		return nil
	}
	return invalid("cell %d is a wide continuation without its wide cell", x)
}

func (r *checkpointReader) cursor() cursor {
	var c cursor
	r.expect(jsontext.KindBeginObject)
	r.cursorFields(&c)
	r.endObject()
	return c
}

func (r *checkpointReader) cursorFields(c *cursor) {
	r.name("row")
	c.y = r.uint(max(r.rows-1, 0))
	r.name("column")
	c.x = r.uint(max(r.columns-1, 0))
	r.name("wrapPending")
	c.wrapPending = r.boolean()
	if c.wrapPending && c.x != r.columns-1 {
		r.failf("pending wrap away from the last column")
	}
}

func (r *checkpointReader) savedCursor() savedCursor {
	var s savedCursor
	r.expect(jsontext.KindBeginObject)
	r.cursorFields(&s.cursor)
	r.name("style")
	s.style = r.style()
	if r.optional("link") {
		s.link = r.link()
	}
	r.name("originMode")
	s.origin = r.boolean()
	r.name("charsets")
	s.charsets = r.charsets()
	r.endObject()
	return s
}

func (r *checkpointReader) margins() region {
	r.expect(jsontext.KindBeginObject)
	r.name("top")
	top := r.uint(max(r.rows-1, 0))
	r.name("bottom")
	bottom := r.uint(max(r.rows-1, 0))
	r.name("left")
	left := r.uint(max(r.columns-1, 0))
	r.name("right")
	right := r.uint(max(r.columns-1, 0))
	r.endObject()
	if top > bottom || left > right {
		r.failf("margins are inverted")
	}
	return region{top: top, bottom: bottom + 1, left: left, right: right + 1}
}

func (r *checkpointReader) charsets() charsets {
	var c charsets
	r.expect(jsontext.KindBeginObject)
	for i := range c.designations {
		r.name(fmt.Sprintf("g%d", i))
		c.designations[i] = charsetName(r.cappedEnum(charsetNames))
	}
	r.name("gl")
	c.gl = r.cappedUint(3)
	r.name("gr")
	c.gr = r.cappedUint(3)
	r.name("singleShift")
	c.single = r.cappedUint(3)
	r.endObject()
	if c.single == 1 {
		r.failf("single shift must be 0, 2 or 3")
	}
	return c
}

func (r *checkpointReader) scrollback() scrollback {
	var s scrollback
	r.expect(jsontext.KindBeginArray)
	for r.more() {
		if s.len() == MaxScrollbackRows {
			r.failf("scrollback has more than %d rows", MaxScrollbackRows)
		}
		kept := r.row().trimmed()
		s.rows = append(s.rows, kept)
		s.bytes += kept.textBytes()
		s.pushed++
	}
	if s.bytes > MaxScrollbackBytes {
		r.failf("scrollback holds %d bytes, more than %d", s.bytes, MaxScrollbackBytes)
	}
	return s
}

func (r *checkpointReader) settings() *Terminal {
	r.name("modes")
	modes := r.modes()
	r.name("tabStops")
	tabs := r.tabStops()
	r.name("charsets")
	sets := r.charsets()
	r.name("cursorStyle")
	r.expect(jsontext.KindBeginObject)
	r.name("shape")
	shape := CursorShape(r.cappedEnum(shapeNames))
	r.name("blink")
	blink := r.boolean()
	r.endObject()
	r.name("cursorVisible")
	if visible := r.boolean(); r.err == nil && visible != modes.has(modeCursorVisible) {
		r.failf("cursorVisible disagrees with DEC mode 25")
	}
	if r.err != nil || r.rows == 0 {
		r.fail(invalid("checkpoint has no screen rows"))
		return nil
	}
	t := newTerminal(r.columns, r.rows)
	t.modes, t.tabs, t.charsets, t.shape, t.blink = modes, tabs, sets, shape, blink
	return t
}

func (r *checkpointReader) modes() modeSet {
	var set modeSet
	r.expect(jsontext.KindBeginObject)
	r.name("ansi")
	set |= r.modeList(false)
	r.name("dec")
	set |= r.modeList(true)
	r.endObject()
	return set
}

func (r *checkpointReader) modeList(dec bool) modeSet {
	var set modeSet
	last := -1
	r.expect(jsontext.KindBeginArray)
	for r.more() {
		n := r.uint(0xffff)
		bit := modeBit(mode{number: uint16(n), dec: dec}) //nolint:gosec // uint bounds n to 0 through 0xffff
		if r.err == nil && (bit == 0 || n <= last) {
			r.failf("mode %d is unsupported or out of order", n)
		}
		last = n
		set |= bit
	}
	return set
}

func (r *checkpointReader) tabStops() []bool {
	tabs := make([]bool, r.columns)
	last := -1
	r.expect(jsontext.KindBeginArray)
	for r.more() {
		x := r.uint(max(r.columns-1, 0))
		if r.err == nil && x <= last {
			r.failf("tab stops are not sorted and unique")
		}
		last = x
		if r.err == nil {
			tabs[x] = true
		}
	}
	return tabs
}

func (r *checkpointReader) continuation(t *Terminal) error {
	r.name("parserStateBase64")
	encoded := r.string(maxParserStateBase64)
	r.endObject()
	if r.err == nil {
		if _, err := r.dec.ReadToken(); !errors.Is(err, io.EOF) {
			r.failf("trailing data after the checkpoint object")
		}
	}
	if r.err != nil {
		return r.err
	}
	data, err := base64.StdEncoding.Strict().DecodeString(encoded)
	if err != nil {
		return invalid("parserStateBase64: %v", err)
	}
	state, err := parseContinuation(data)
	if err != nil {
		return err
	}
	t.replaying = true
	t.Feed(state.sequence)
	t.replaying = false
	t.cluster = append(t.cluster[:0], state.cluster...)
	t.lastChar = state.lastChar
	return nil
}
