package vtstate

import (
	"bytes"
	"encoding/base64"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"runtime/debug"
	"strings"
	"sync"
)

const (
	Version            = 1
	MaxCheckpointBytes = 8 << 20
	engineRevision     = "x/vt@43427fdbad12+vtstate.1"
)

var (
	ErrIncompatible      = errors.New("vtstate: checkpoint is incompatible with this build")
	ErrInvalidCheckpoint = errors.New("vtstate: checkpoint body is invalid")
	ErrLimitExceeded     = errors.New("vtstate: checkpoint exceeds a declared limit")
)

func invalid(format string, args ...any) error {
	return fmt.Errorf("%w: "+format, append([]any{ErrInvalidCheckpoint}, args...)...)
}

var engineModules = [...]string{
	"github.com/charmbracelet/x/ansi",
	"github.com/clipperhouse/displaywidth",
	"github.com/clipperhouse/uax29/v2",
}

var engineVersion = sync.OnceValue(func() string {
	versions := make(map[string]string, len(engineModules))
	if info, ok := debug.ReadBuildInfo(); ok {
		for _, dep := range info.Deps {
			versions[dep.Path] = dep.Version
		}
	}
	var b strings.Builder
	b.WriteString(engineRevision)
	for _, path := range engineModules {
		version := versions[path]
		if version == "" {
			version = "unknown"
		}
		b.WriteString(" " + strings.TrimPrefix(path, "github.com/") + "@" + version)
	}
	return b.String()
})

func EngineVersion() string {
	return engineVersion()
}

var (
	screenNames    = []string{"primary", "alternate"}
	underlineNames = []string{"none", "single", "double", "curly", "dotted", "dashed"}
	attributeNames = []string{"bold", "faint", "italic", "blink", "rapidBlink", "reverse", "conceal", "strikethrough"}
	shapeNames     = []string{"block", "underline", "bar"}
	charsetNames   = []string{"ascii", "uk", "special"}
	colorKindNames = []string{"default", "indexed", "rgb"}
)

type tables struct {
	styleIDs map[Style]int
	linkIDs  map[Link]int
	styles   []Style
	links    []Link
}

func newTables() *tables {
	return &tables{styleIDs: map[Style]int{}, linkIDs: map[Link]int{}}
}

func (tb *tables) style(s Style) int {
	if id, ok := tb.styleIDs[s]; ok {
		return id
	}
	tb.styleIDs[s] = len(tb.styles)
	tb.styles = append(tb.styles, s)
	return len(tb.styles) - 1
}

func (tb *tables) link(l Link) int {
	if id, ok := tb.linkIDs[l]; ok {
		return id
	}
	tb.linkIDs[l] = len(tb.links)
	tb.links = append(tb.links, l)
	return len(tb.links) - 1
}

func (tb *tables) truncate(styles, links int) {
	for _, s := range tb.styles[styles:] {
		delete(tb.styleIDs, s)
	}
	for _, l := range tb.links[links:] {
		delete(tb.linkIDs, l)
	}
	tb.styles = tb.styles[:styles]
	tb.links = tb.links[:links]
}

func (tb *tables) cell(c Cell) {
	tb.style(c.Style)
	if c.Link != nil {
		tb.link(*c.Link)
	}
}

func (tb *tables) saved(s savedCursor) {
	tb.style(s.style)
	if s.link != nil {
		tb.link(*s.link)
	}
}

type bodyWriter struct {
	enc *jsontext.Encoder
	tb  *tables
	err error
}

func newBodyWriter(buf *bytes.Buffer, tb *tables) *bodyWriter {
	return &bodyWriter{enc: jsontext.NewEncoder(buf, jsontext.AllowDuplicateNames(true)), tb: tb}
}

func (w *bodyWriter) token(t jsontext.Token) {
	if w.err == nil {
		w.err = w.enc.WriteToken(t)
	}
}

func (w *bodyWriter) name(n string) {
	w.token(jsontext.String(n))
}

func (w *bodyWriter) uint(n string, v int) {
	w.name(n)
	if v < 0 {
		if w.err == nil {
			w.err = invalid("value for %q is negative: %d", n, v)
		}
		return
	}
	w.token(jsontext.Uint(uint64(v))) //nolint:gosec // v is checked non-negative above
}

func (w *bodyWriter) str(n, v string) {
	w.name(n)
	w.token(jsontext.String(v))
}

func (w *bodyWriter) boolean(n string, v bool) {
	w.name(n)
	w.token(jsontext.Bool(v))
}

func (t *Terminal) Checkpoint() ([]byte, error) {
	continuation, err := t.continuation()
	if err != nil {
		return nil, err
	}
	parserState := base64.StdEncoding.EncodeToString(continuation)
	return t.boundedCheckpoint(parserState)
}

func (t *Terminal) boundedCheckpoint(parserState string) ([]byte, error) {
	tb := newTables()
	t.registerFixed(tb)
	fixed, err := t.encode(tb, parserState, 0)
	if err != nil {
		return nil, err
	}
	if len(fixed) > MaxCheckpointBytes {
		return nil, fmt.Errorf("%w: screen state needs %d bytes, more than %d", ErrLimitExceeded, len(fixed), MaxCheckpointBytes)
	}
	kept, err := t.selectScrollback(tb, MaxCheckpointBytes-len(fixed))
	if err != nil {
		return nil, err
	}
	body, err := t.encode(tb, parserState, kept)
	if err != nil {
		return nil, err
	}
	if len(body) > MaxCheckpointBytes {
		return nil, fmt.Errorf("%w: checkpoint needs %d bytes, more than %d", ErrLimitExceeded, len(body), MaxCheckpointBytes)
	}
	return body, nil
}

func (t *Terminal) registerFixed(tb *tables) {
	tb.style(t.style)
	if t.link != nil {
		tb.link(*t.link)
	}
	for i := range t.screens {
		for _, r := range t.screens[i].rows {
			for _, c := range r.cells {
				tb.cell(c)
			}
		}
		tb.saved(t.screens[i].saved)
	}
}

func (t *Terminal) selectScrollback(tb *tables, budget int) (int, error) {
	var buf bytes.Buffer
	w := newBodyWriter(&buf, tb)
	columns := t.screens[0].columns()
	kept := 0
	for i := t.history.len() - 1; i >= 0; i-- {
		styles, links := len(tb.styles), len(tb.links)
		buf.Reset()
		w.enc.Reset(&buf, jsontext.AllowDuplicateNames(true))
		w.row(t.history.row(i), columns)
		w.tableEntries(styles, links)
		if w.err != nil {
			return 0, w.err
		}
		cost := int(w.enc.OutputOffset()) + 1 + (len(tb.styles) - styles) + (len(tb.links) - links)
		if cost > budget {
			tb.truncate(styles, links)
			break
		}
		budget -= cost
		kept++
	}
	return kept, nil
}

func (w *bodyWriter) tableEntries(styles, links int) {
	for _, s := range w.tb.styles[styles:] {
		w.style(s)
	}
	for _, l := range w.tb.links[links:] {
		w.linkRecord(l)
	}
}

func (t *Terminal) encode(tb *tables, parserState string, scrollbackRows int) ([]byte, error) {
	var buf bytes.Buffer
	w := newBodyWriter(&buf, tb)
	w.token(jsontext.BeginObject)
	w.uint("version", Version)
	w.str("engineVersion", EngineVersion())
	w.name("styles")
	w.token(jsontext.BeginArray)
	for _, s := range tb.styles {
		w.style(s)
	}
	w.token(jsontext.EndArray)
	w.name("links")
	w.token(jsontext.BeginArray)
	for _, l := range tb.links {
		w.linkRecord(l)
	}
	w.token(jsontext.EndArray)
	w.uint("currentStyle", tb.styleIDs[t.style])
	if t.link != nil {
		w.uint("currentLink", tb.linkIDs[*t.link])
	}
	w.str("activeScreen", screenNames[t.active])
	w.name("primary")
	w.screen(&t.screens[0])
	w.name("alternate")
	w.screen(&t.screens[1])
	w.name("scrollback")
	w.token(jsontext.BeginArray)
	columns := t.screens[0].columns()
	for i := t.history.len() - scrollbackRows; i < t.history.len(); i++ {
		w.row(t.history.row(i), columns)
	}
	w.token(jsontext.EndArray)
	w.uint("droppedScrollbackRows", t.droppedScrollback+t.history.len()-scrollbackRows)
	t.encodeSettings(w)
	w.str("parserStateBase64", parserState)
	w.token(jsontext.EndObject)
	if w.err != nil {
		return nil, fmt.Errorf("vtstate: encode checkpoint: %w", w.err)
	}
	return buf.Bytes(), nil
}

func (t *Terminal) encodeSettings(w *bodyWriter) {
	w.name("modes")
	w.token(jsontext.BeginObject)
	w.modeList("ansi", t.modes, false)
	w.modeList("dec", t.modes, true)
	w.token(jsontext.EndObject)
	w.name("tabStops")
	w.token(jsontext.BeginArray)
	for x, stop := range t.tabs {
		if stop {
			w.token(jsontext.Uint(uint64(x)))
		}
	}
	w.token(jsontext.EndArray)
	w.name("charsets")
	w.charsets(t.charsets)
	w.name("cursorStyle")
	w.token(jsontext.BeginObject)
	w.str("shape", shapeNames[t.shape])
	w.boolean("blink", t.blink)
	w.token(jsontext.EndObject)
	w.boolean("cursorVisible", t.modes.has(modeCursorVisible))
}

func (w *bodyWriter) modeList(n string, modes modeSet, dec bool) {
	w.name(n)
	w.token(jsontext.BeginArray)
	for _, m := range sortedModes(dec) {
		if modes.has(modeBit(m)) {
			w.token(jsontext.Uint(uint64(m.number)))
		}
	}
	w.token(jsontext.EndArray)
}

func sortedModes(dec bool) []mode {
	var list []mode
	for _, m := range supportedModes {
		if m.dec == dec {
			list = append(list, m)
		}
	}
	return list
}

func (w *bodyWriter) screen(s *screen) {
	columns := s.columns()
	w.token(jsontext.BeginObject)
	w.name("rows")
	w.token(jsontext.BeginArray)
	for _, r := range s.rows {
		w.row(r, columns)
	}
	w.token(jsontext.EndArray)
	w.name("cursor")
	w.token(jsontext.BeginObject)
	w.cursorFields(s.cursor)
	w.token(jsontext.EndObject)
	w.name("savedCursor")
	w.token(jsontext.BeginObject)
	w.cursorFields(s.saved.cursor)
	w.uint("style", w.tb.style(s.saved.style))
	if s.saved.link != nil {
		w.uint("link", w.tb.link(*s.saved.link))
	}
	w.boolean("originMode", s.saved.origin)
	w.name("charsets")
	w.charsets(s.saved.charsets)
	w.token(jsontext.EndObject)
	w.name("margins")
	w.token(jsontext.BeginObject)
	w.uint("top", s.scroll.top)
	w.uint("bottom", s.scroll.bottom-1)
	w.uint("left", s.scroll.left)
	w.uint("right", s.scroll.right-1)
	w.token(jsontext.EndObject)
	w.token(jsontext.EndObject)
}

func (w *bodyWriter) cursorFields(c cursor) {
	w.uint("row", c.y)
	w.uint("column", c.x)
	w.boolean("wrapPending", c.wrapPending)
}

func (w *bodyWriter) row(r row, columns int) {
	w.token(jsontext.BeginObject)
	w.name("cells")
	w.token(jsontext.BeginArray)
	for _, c := range r.cells {
		w.cell(c)
	}
	for range columns - len(r.cells) {
		w.cell(blankCell(Color{}))
	}
	w.token(jsontext.EndArray)
	w.boolean("softWrapped", r.wrapped)
	w.token(jsontext.EndObject)
}

func (w *bodyWriter) cell(c Cell) {
	w.token(jsontext.BeginObject)
	w.str("text", c.Text)
	w.uint("width", int(c.Width))
	w.uint("style", w.tb.style(c.Style))
	if c.Link != nil {
		w.uint("link", w.tb.link(*c.Link))
	}
	w.token(jsontext.EndObject)
}

func (w *bodyWriter) style(s Style) {
	w.token(jsontext.BeginObject)
	w.name("foreground")
	w.color(s.Foreground)
	w.name("background")
	w.color(s.Background)
	w.name("underlineColor")
	w.color(s.UnderlineColor)
	w.str("underline", underlineNames[s.Underline])
	w.name("attributes")
	w.token(jsontext.BeginArray)
	for i, n := range attributeNames {
		if s.Attributes&(1<<i) != 0 {
			w.token(jsontext.String(n))
		}
	}
	w.token(jsontext.EndArray)
	w.token(jsontext.EndObject)
}

func (w *bodyWriter) color(c Color) {
	w.token(jsontext.BeginObject)
	w.str("kind", colorKindNames[c.Kind])
	switch c.Kind {
	case ColorDefault:
	case ColorIndexed:
		w.uint("index", int(c.Index))
	case ColorRGB:
		w.uint("red", int(c.Red))
		w.uint("green", int(c.Green))
		w.uint("blue", int(c.Blue))
	}
	w.token(jsontext.EndObject)
}

func (w *bodyWriter) linkRecord(l Link) {
	w.token(jsontext.BeginObject)
	w.str("uri", l.URI)
	w.str("id", l.ID)
	w.token(jsontext.EndObject)
}

func (w *bodyWriter) charsets(c charsets) {
	w.token(jsontext.BeginObject)
	for i, d := range c.designations {
		w.str(fmt.Sprintf("g%d", i), charsetNames[d])
	}
	w.uint("gl", int(c.gl))
	w.uint("gr", int(c.gr))
	w.uint("singleShift", int(c.single))
	w.token(jsontext.EndObject)
}
