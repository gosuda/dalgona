package tui

import (
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
	m := newModel(nil, Options{}, nil)
	if m.screen.method != ansi.WcWidth {
		t.Fatalf("start method = %v, want WcWidth", m.screen.method)
	}
	m, _ = drive(t, m, tea.ModeReportMsg{Mode: ansi.ModeUnicodeCore, Value: ansi.ModeReset})
	if m.screen.method != ansi.GraphemeWidth {
		t.Fatalf("method after mode report = %v, want GraphemeWidth", m.screen.method)
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
