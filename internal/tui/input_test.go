package tui

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

func TestEditorNewlineKeys(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m.input.composer.SetValue("a")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter, Mod: tea.ModShift})
	m, _ = drive(t, m, tea.KeyPressMsg{Code: 'j', Mod: tea.ModCtrl})
	if got := m.input.composer.Value(); got != "a\n\n" {
		t.Fatalf("shift+enter and ctrl+j inserted %q, want two newlines after a", got)
	}
	m.input.composer.SetValue(`b\`)
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	if got := m.input.composer.Value(); got != "b\n" {
		t.Fatalf("enter after a backslash left %q, want the backslash replaced by a newline", got)
	}
	if m.notice != "" {
		t.Fatalf("enter after a backslash submitted the prompt: notice %q", m.notice)
	}
}

func TestEditorCursorPosition(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 8})
	m.input.composer.SetValue("ab")
	m.input.composer.cursor = 1
	view := m.View()
	if view.Cursor == nil {
		t.Fatal("the editor left the terminal cursor unset")
	}
	if view.Cursor.X != 3 || view.Cursor.Y != 0 {
		t.Fatalf("cursor at (%d, %d), want (3, 0) after the prompt mark and one letter", view.Cursor.X, view.Cursor.Y)
	}
}

func TestEditorWrapsAtWidthMinusOne(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 12})
	m.input.composer.SetValue(strings.Repeat("a", 100))
	for _, line := range strings.Split(stripANSI(m.View().Content), "\n") {
		if got := m.screen.cells(line); got > 39 {
			t.Fatalf("editor row is %d cells at 40 columns: %q", got, line)
		}
	}
}

func TestEditorScrollsInsideBeyondCap(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 8})
	var lines []string
	var heights []int
	for i := 1; i <= 10; i++ {
		lines = append(lines, fmt.Sprintf("line %d", i))
		m.input.composer.SetValue(strings.Join(lines, "\n"))
		heights = append(heights, len(m.dockLines()))
	}
	if heights[9] != heights[5] {
		t.Fatalf("dock height grew past the editor cap: %d lines after six, %d after ten", heights[5], heights[9])
	}
	view := m.View()
	editorRows := heights[9] - 2
	if view.Cursor == nil || view.Cursor.Y < 0 || view.Cursor.Y >= editorRows {
		t.Fatalf("cursor row %v is outside the %d editor rows", view.Cursor, editorRows)
	}
	if !strings.Contains(stripANSI(view.Content), "line 10") {
		t.Fatalf("the editor does not show the caret line:\n%s", stripANSI(view.Content))
	}
}

func TestEditorLineKeys(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name   string
		text   string
		cursor int
		key    tea.KeyPressMsg
		want   string
		at     int
	}{
		{name: "ctrl+w deletes the word before the caret", text: "one two\nthree", cursor: 13, key: tea.KeyPressMsg{Code: 'w', Mod: tea.ModCtrl}, want: "one two\n", at: 8},
		{name: "ctrl+u kills to the start of the line", text: "one\ntwo", cursor: 6, key: tea.KeyPressMsg{Code: 'u', Mod: tea.ModCtrl}, want: "one\no", at: 4},
		{name: "ctrl+k kills to the end of the line", text: "one\ntwo", cursor: 5, key: tea.KeyPressMsg{Code: 'k', Mod: tea.ModCtrl}, want: "one\nt", at: 5},
		{name: "ctrl+k at the end of a line deletes nothing", text: "one\ntwo", cursor: 3, key: tea.KeyPressMsg{Code: 'k', Mod: tea.ModCtrl}, want: "one\ntwo", at: 3},
		{name: "home goes to the start of the line", text: "one\ntwo", cursor: 6, key: tea.KeyPressMsg{Code: tea.KeyHome}, want: "one\ntwo", at: 4},
		{name: "end goes to the end of the line", text: "one\ntwo", cursor: 4, key: tea.KeyPressMsg{Code: tea.KeyEnd}, want: "one\ntwo", at: 7},
		{name: "alt+left moves to the previous word", text: "one two", cursor: 7, key: tea.KeyPressMsg{Code: tea.KeyLeft, Mod: tea.ModAlt}, want: "one two", at: 4},
		{name: "alt+right moves past the next word", text: "one two", cursor: 0, key: tea.KeyPressMsg{Code: tea.KeyRight, Mod: tea.ModAlt}, want: "one two", at: 3},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			m := newTestModel(nil, Options{}, nil)
			m.input.composer.SetValue(test.text)
			m.input.composer.cursor = test.cursor
			m, _ = drive(t, m, test.key)
			if got := m.input.composer.Value(); got != test.want {
				t.Fatalf("text %q, want %q", got, test.want)
			}
			if m.input.composer.cursor != test.at {
				t.Fatalf("caret at %d, want %d", m.input.composer.cursor, test.at)
			}
		})
	}
}

func TestEditorUpAndDownMoveBetweenWrappedRows(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 12})
	m.input.composer.SetValue(strings.Repeat("a", 100))
	m.input.composer.cursor = 40
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyUp})
	if m.input.composer.cursor != 3 {
		t.Fatalf("up from row 1 column 3 left the caret at %d, want 3", m.input.composer.cursor)
	}
	m.input.composer.cursor = 40
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	if m.input.composer.cursor != 77 {
		t.Fatalf("down from row 1 column 3 left the caret at %d, want 77", m.input.composer.cursor)
	}
}

func TestEditorDecomposedTextWrapsAsComposed(t *testing.T) {
	t.Parallel()
	const units = 30
	tests := []struct {
		name      string
		caret     int
		caretComp int
	}{
		{name: "caret at the end", caret: 3 * units, caretComp: 2 * units},
		{name: "caret after the 40th character", caret: 60, caretComp: 40},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			render := func(text string, caret int) (string, *tea.Cursor) {
				m := newTestModel(nil, Options{}, nil)
				m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 12})
				m.input.composer.SetValue(text)
				m.input.composer.cursor = caret
				view := m.View()
				return stripANSI(view.Content), view.Cursor
			}
			wantText, wantCursor := render(strings.Repeat("aé", units), test.caretComp)
			gotText, gotCursor := render(strings.Repeat("ae\u0301", units), test.caret)
			if gotText != wantText {
				t.Fatalf("decomposed text renders differently:\n%s\nwant:\n%s", gotText, wantText)
			}
			if gotCursor == nil || wantCursor == nil {
				t.Fatalf("cursor unset: decomposed %v, composed %v", gotCursor, wantCursor)
			}
			if gotCursor.X != wantCursor.X || gotCursor.Y != wantCursor.Y {
				t.Fatalf("decomposed caret at (%d, %d), composed at (%d, %d)", gotCursor.X, gotCursor.Y, wantCursor.X, wantCursor.Y)
			}
		})
	}
}

func TestEditorFollowsRendererWidthMode(t *testing.T) {
	t.Parallel()
	const family = "\U0001F468\u200D\U0001F469\u200D\U0001F467"
	m := newTestModel(nil, Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 12})
	m.input.composer.SetValue(family)
	before := m.View().Cursor
	m, _ = drive(t, m, tea.ModeReportMsg{Mode: ansi.ModeUnicodeCore, Value: ansi.ModeSet})
	after := m.View().Cursor
	if before == nil || after == nil {
		t.Fatalf("cursor unset: before %v, after %v", before, after)
	}
	if before.X != 2+6 || after.X != 2+2 {
		t.Fatalf("caret after the family emoji at X %d before the mode report and %d after, want 8 and 4", before.X, after.X)
	}
}
