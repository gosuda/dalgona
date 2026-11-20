package tui

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

func fieldWindowProblem(screen *screenState, text []rune, cursor, room int) string {
	window, caret := fieldWindow(screen, text, cursor, room)
	if got := screen.cells(window); got > room || caret > got {
		return fmt.Sprintf("is (%q, %d) of %d cells, want at most %d with the caret inside", window, caret, got, room)
	}
	if cursor == len(text) && !strings.HasSuffix(strings.TrimRight(window, " "), string(text[len(text)-1])) {
		return fmt.Sprintf("is %q, want the end of the draft with the caret at its end", window)
	}
	return ""
}

func fieldWindowSweepProblem(screen *screenState, draft string) string {
	text := []rune(draft)
	for room := 8; room <= 45; room++ {
		for cursor := 0; cursor <= len(text); cursor++ {
			if problem := fieldWindowProblem(screen, text, cursor, room); problem != "" {
				return fmt.Sprintf("fieldWindow(%q, %d, %d) %s", draft, cursor, room, problem)
			}
		}
	}
	return ""
}

func TestFieldWindowNeverPrintsPastTheRoom(t *testing.T) {
	t.Parallel()
	drafts := []string{
		"abcdefghij\tklmnopqrst\tuvw",
		"abcdefghijklmnopqrstuvwxyz\t0123456789",
		"\t\tab\tcd\tef\tgh\t\ti",
	}
	for _, method := range []ansi.Method{ansi.WcWidth, ansi.GraphemeWidth} {
		screen := screenState{method: method}
		for _, draft := range drafts {
			if problem := fieldWindowSweepProblem(&screen, draft); problem != "" {
				t.Fatalf("method %v %s", method, problem)
			}
		}
	}
}

func TestFilterFieldPrintsTabbedPasteInsideTheWindow(t *testing.T) {
	t.Parallel()
	cases := []struct {
		width int
		draft string
		tail  string
	}{
		{40, "abcdefghij\tklmnopqrst\tuvw", "uvw"},
		{30, "abcdefghijklmnopqrstuvwxyz\t0123456789", "0123456789"},
	}
	for _, tc := range cases {
		m := newTestModel(nil, Options{})
		m, _ = drive(t, m, tea.WindowSizeMsg{Width: tc.width, Height: 24})
		m, _ = drive(t, m, press('r', tea.ModCtrl, ""))
		m, _ = drive(t, m, tea.PasteMsg{Content: tc.draft})
		view := m.View()
		lines := strings.Split(stripANSI(view.Content), "\n")
		assertFrame(t, lines, tc.width, 24)
		row := ""
		for _, line := range lines {
			if strings.HasPrefix(line, "filter: ") {
				row = line
			}
		}
		if !strings.HasSuffix(strings.TrimRight(row, " "), tc.tail) {
			t.Fatalf("width %d filter row = %q, want it to end with %q", tc.width, row, tc.tail)
		}
		if view.Cursor == nil || view.Cursor.X != m.screen.cells(strings.TrimRight(row, " ")) {
			t.Fatalf("width %d cursor = %v, want the column after the row %q", tc.width, view.Cursor, row)
		}
	}
}
