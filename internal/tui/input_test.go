package tui

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/rpc"
)

func TestEditorNewlineKeys(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
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
	m := newTestModel(nil, Options{})
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
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 12})
	m.input.composer.SetValue(strings.Repeat("a", 100))
	for line := range strings.SplitSeq(stripANSI(m.View().Content), "\n") {
		if got := m.screen.cells(line); got > 39 {
			t.Fatalf("editor row is %d cells at 40 columns: %q", got, line)
		}
	}
}

func TestEditorScrollsInsideBeyondCap(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
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
			m := newTestModel(nil, Options{})
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
	m := newTestModel(nil, Options{})
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
				m := newTestModel(nil, Options{})
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
	m := newTestModel(nil, Options{})
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

func pasteLines(n int) string {
	lines := make([]string, n)
	for i := range lines {
		lines[i] = fmt.Sprintf("line %02d", i+1)
	}
	return strings.Join(lines, "\n")
}

func TestLargePasteBecomesChipAndExpandsOnSend(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, tea.PasteMsg{Content: pasteLines(12)})
	if !strings.Contains(dockText(m), "[Paste #1 +12 lines]") {
		t.Fatalf("composer does not show the paste chip:\n%s", dockText(m))
	}
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	runCommand(t, m, command, "Enter did not send the prompt")
	if len(caller.calls) == 0 || caller.calls[0].method != "turn/start" {
		t.Fatalf("calls = %#v, want turn/start", caller.calls)
	}
	input := caller.calls[0].params.(rpc.TurnStartParams).Input
	if len(input) != 1 || input[0].Text != pasteLines(12) {
		t.Fatalf("sent %q, want the twelve pasted lines", input)
	}
}

func TestChipPromptIsSentUnchangedWhenSlashLike(t *testing.T) {
	for _, first := range []string{"// x", "/model foo"} {
		caller := new(fakeCaller)
		m := newModel(caller, Options{}, nil)
		m.thread.id = "t1"
		m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
		content := first + "\n" + pasteLines(11)
		m, _ = drive(t, m, tea.PasteMsg{Content: content})
		m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
		runCommand(t, m, command, "Enter did not send the prompt for "+first)
		if len(caller.calls) == 0 || caller.calls[0].method != "turn/start" {
			t.Fatalf("%q: calls = %#v, want turn/start only", first, caller.calls)
		}
		input := caller.calls[0].params.(rpc.TurnStartParams).Input
		if len(input) != 1 || input[0].Text != content {
			t.Fatalf("%q: sent %#v, want the paste unchanged", first, input)
		}
	}
}

func TestChipDeletesAsUnit(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, tea.PasteMsg{Content: pasteLines(12)})
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyBackspace})
	if got := m.input.composer.Value(); got != "" {
		t.Fatalf("backspace left %q, want the whole chip removed", got)
	}
	if strings.Contains(dockText(m), "[Paste #") {
		t.Fatalf("chip label survived backspace:\n%s", dockText(m))
	}
}

func TestOversizePasteRefused(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, tea.PasteMsg{Content: strings.Repeat("a", 1<<20+1)})
	want := "Paste is 1048577 bytes, over the 1 MiB limit; save it to a file and mention it with @path"
	if m.notice != want {
		t.Fatalf("notice = %q, want %q", m.notice, want)
	}
	if got := m.input.composer.Value(); got != "" {
		t.Fatalf("refused paste reached the composer: %d bytes", len(got))
	}
}

func TestPasteIntoFilterKeepsFirstLine(t *testing.T) {
	t.Parallel()
	m := openModelPicker(t, new(fakeCaller))
	m, _ = drive(t, m, tea.PasteMsg{Content: "one\ntwo"})
	if got := m.input.field.value(); got != "one" {
		t.Fatalf("filter holds %q, want the first line only", got)
	}
}

func TestChipRowDoesNotGrowComposer(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	before := len(m.dockLines())
	m, _ = drive(t, m, tea.PasteMsg{Content: pasteLines(40)})
	if got := len(m.dockLines()); got != before || got != 3 {
		t.Fatalf("dock is %d rows after a 40-line paste, want %d (composer, live row, status band)", got, before)
	}
}
