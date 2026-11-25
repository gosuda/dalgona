//go:build gui

package decision

import (
	"unicode/utf8"

	"gioui.org/io/input"
	"gioui.org/io/key"
)

var noComposition = key.Range{Start: -1, End: -1}

type inputMethod struct {
	router  *input.Router
	state   input.EditorState
	compose key.Range
}

func ordered(r key.Range) key.Range {
	if r.Start > r.End {
		r.Start, r.End = r.End, r.Start
	}
	return r
}

func (m *inputMethod) target() key.Range {
	if m.compose.Start == -1 {
		return ordered(m.state.Selection.Range)
	}
	return ordered(m.compose)
}

func (m *inputMethod) replace(r key.Range, s string) {
	m.router.Queue(key.EditEvent{Range: r, Text: s})
}

func (m *inputMethod) caret(at int) {
	m.state.Selection.Range = key.Range{Start: at, End: at}
	m.router.Queue(key.SelectionEvent(m.state.Selection.Range))
}

func (m *inputMethod) mark(r key.Range) {
	m.compose = r
	m.router.Queue(key.CompositionEvent(r))
}

func (m *inputMethod) insert(s string) {
	r := m.target()
	m.replace(r, s)
	m.caret(r.Start + utf8.RuneCountInString(s))
}

func (m *inputMethod) composition(preedit string) {
	r := m.target()
	end := r.Start + utf8.RuneCountInString(preedit)
	m.replace(r, preedit)
	m.mark(key.Range{Start: r.Start, End: end})
	m.caret(end)
}

func (m *inputMethod) result(committed string) {
	r := m.target()
	end := r.Start + utf8.RuneCountInString(committed)
	m.replace(r, committed)
	m.mark(noComposition)
	m.caret(end)
}
