package tui

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

func TestCellsAndFitTable(t *testing.T) {
	t.Parallel()
	screen := screenState{method: ansi.GraphemeWidth}
	cases := []struct {
		in    string
		cells int
		fit   string
	}{
		{"abc", 3, "abc"},
		{"abcdef", 6, "ab..."},
		{"한글", 4, "한글"},
		{"한글한글", 8, "한..."},
		{"α", 1, "α"},
		{"e\u0301", 1, "é"},
	}
	for _, tc := range cases {
		if got := screen.cells(tc.in); got != tc.cells {
			t.Errorf("cells(%q) = %d, want %d", tc.in, got, tc.cells)
		}
		if got := screen.fit(tc.in, 5); got != tc.fit {
			t.Errorf("fit(%q, 5) = %q, want %q", tc.in, got, tc.fit)
		}
	}
}

func TestCellsFamilyEmojiFollowsMethod(t *testing.T) {
	t.Parallel()
	const family = "\U0001F468\u200d\U0001F469\u200d\U0001F467"
	grapheme := screenState{method: ansi.GraphemeWidth}
	if got := grapheme.cells(family); got != 2 {
		t.Errorf("GraphemeWidth cells(family) = %d, want 2", got)
	}
	wc := screenState{method: ansi.WcWidth}
	if got := wc.cells(family); got != 6 {
		t.Errorf("WcWidth cells(family) = %d, want 6", got)
	}
}

func TestCellsCountsAmbiguousAsOne(t *testing.T) {
	t.Parallel()
	for _, method := range []ansi.Method{ansi.WcWidth, ansi.GraphemeWidth} {
		screen := screenState{method: method}
		if got := screen.cells("●"); got != 1 {
			t.Errorf("method %v cells(●) = %d, want 1", method, got)
		}
	}
}

func TestScreenMethodFollowsModeReport(t *testing.T) {
	t.Parallel()
	for _, value := range []ansi.ModeSetting{ansi.ModeReset, ansi.ModeSet, ansi.ModePermanentlySet} {
		m := newModel(nil, Options{}, nil)
		m, _ = drive(t, m, tea.ModeReportMsg{Mode: ansi.ModeUnicodeCore, Value: value})
		if m.screen.method != ansi.GraphemeWidth {
			t.Fatalf("method after UnicodeCore mode report %v = %v, want GraphemeWidth", value, m.screen.method)
		}
	}
	for _, value := range []ansi.ModeSetting{ansi.ModeSet, ansi.ModePermanentlyReset} {
		m := newModel(nil, Options{}, nil)
		m, _ = drive(t, m, tea.ModeReportMsg{Mode: ansi.ModeInsertReplace, Value: value})
		if m.screen.method != ansi.WcWidth {
			t.Fatalf("method after InsertReplace mode report %v = %v, want WcWidth", value, m.screen.method)
		}
	}
}

func TestWrapSplitsByCells(t *testing.T) {
	t.Parallel()
	screen := screenState{method: ansi.GraphemeWidth}
	got := screen.wrap("abcdef", 3)
	if len(got) != 2 || got[0] != "abc" || got[1] != "def" {
		t.Fatalf("wrap(abcdef, 3) = %q, want [abc def]", got)
	}
}

func TestCellsCountsTabStops(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name  string
		in    string
		cells int
	}{
		{"tab at the start", "\tx", 9},
		{"tab after two letters", "ab\tx", 9},
		{"tab after a full stop", "12345678\tx", 17},
		{"tab after seven letters", "1234567\tx", 9},
		{"two tabs", "\t\t", 16},
		{"tab after a wide rune", "한\tx", 9},
		{"tab after four wide runes", "한글한글\tx", 17},
		{"tab after a combining mark", "é\tx", 9},
		{"tab after a colour code", "\x1b[31mab\x1b[0m\tx", 9},
		{"tab inside a colour code span", "\x1b[31m\tx\x1b[0m", 9},
		{"no tab", "ab", 2},
	}
	for _, method := range []ansi.Method{ansi.WcWidth, ansi.GraphemeWidth} {
		screen := screenState{method: method}
		for _, tc := range cases {
			if got := screen.cells(tc.in); got != tc.cells {
				t.Errorf("method %v %s: cells(%q) = %d, want %d", method, tc.name, tc.in, got, tc.cells)
			}
		}
	}
}

func TestFitPrintsSpacesForTabs(t *testing.T) {
	t.Parallel()
	cases := []struct {
		in   string
		n    int
		want string
	}{
		{"a\tb", 20, "a       b"},
		{"\tx", 20, "        x"},
		{"\t\tabc", 10, "       ..."},
		{"12345678\tx", 12, "12345678 ..."},
	}
	for _, method := range []ansi.Method{ansi.WcWidth, ansi.GraphemeWidth} {
		screen := screenState{method: method}
		for _, tc := range cases {
			if got := screen.fit(tc.in, tc.n); got != tc.want {
				t.Errorf("method %v fit(%q, %d) = %q, want %q", method, tc.in, tc.n, got, tc.want)
			}
		}
	}
}

func TestWrapPrintsSpacesForTabs(t *testing.T) {
	t.Parallel()
	for _, method := range []ansi.Method{ansi.WcWidth, ansi.GraphemeWidth} {
		screen := screenState{method: method}
		for _, n := range []int{9, 12, 17, 24, 39} {
			requireRowsWithoutTabs(t, &screen, screen.wrap("\t\tab\tcdefghijklmnop\n\tqrstuvwxyz", n), n)
		}
	}
}

func requireRowsWithoutTabs(t *testing.T, screen *screenState, rows []string, n int) {
	t.Helper()
	for _, row := range rows {
		if strings.Contains(row, "\t") {
			t.Errorf("wrap at %d kept a tab: %q", n, row)
		}
		if got := screen.cells(row); got > n {
			t.Errorf("wrap at %d made a %d-cell row: %q", n, got, row)
		}
	}
}

func TestWrapRestartsTabStopsAfterNewline(t *testing.T) {
	t.Parallel()
	for _, method := range []ansi.Method{ansi.WcWidth, ansi.GraphemeWidth} {
		screen := screenState{method: method}
		rows := screen.wrap("ab\n\tc", 40)
		if len(rows) != 2 || rows[0] != "ab" || rows[1] != "        c" {
			t.Errorf("method %v wrap(%q, 40) = %q, want the second line to start its stops at its own first cell", method, "ab\n\tc", rows)
		}
	}
}
