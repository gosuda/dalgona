package tui

import (
	"fmt"
	"strings"
	"testing"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

func TestFieldWindowShowsWholeClustersAndEscapes(t *testing.T) {
	t.Parallel()
	draft := strings.Repeat("e\u0301\U0001F468\u200D\U0001F469x\u0007", 3)
	bounds := inputtext.Boundaries(draft)
	shown := make([]string, 0, len(bounds)-1)
	for i := range bounds[1:] {
		shown = append(shown, displayText(draft[bounds[i]:bounds[i+1]]))
	}
	screen := screenState{method: ansi.GraphemeWidth}
	for room := 10; room <= 24; room++ {
		for _, at := range bounds {
			window, _ := fieldWindow(&screen, []rune(draft), utf8.RuneCountInString(draft[:at]), room)
			inner := strings.TrimSuffix(strings.TrimPrefix(window, "..."), "...")
			if !joinsWholeClusters(shown, inner) {
				t.Fatalf("fieldWindow at byte %d in %d cells = %q, want whole clusters with whole escapes", at, room, window)
			}
		}
	}
}

func joinsWholeClusters(shown []string, inner string) bool {
	for first := range shown {
		joined := ""
		for _, cluster := range shown[first:] {
			if joined += cluster; joined == inner {
				return true
			}
		}
	}
	return inner == ""
}

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
