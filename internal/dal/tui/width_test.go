package tui

import (
	"fmt"
	"slices"
	"strings"
	"testing"
	"unicode"

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

func TestWrapKeepsEveryTabbedRowWithinTheWidth(t *testing.T) {
	t.Parallel()
	texts := []string{
		"abcdefghijklmnopq ab\t12345678901",
		"the quick brown fox jumps over the lazy dog\tand\tthen\t\tsome more words\there",
		"\tabcdefghijklmnopqrstuvwxyz0123456789",
		"ab\tabcdefghijklmnopqrstuvwxyz0123456789 \tx",
		"a\t\t\tb\t\tc \t\t d\t",
	}
	for _, method := range []ansi.Method{ansi.WcWidth, ansi.GraphemeWidth} {
		screen := screenState{method: method}
		for _, text := range texts {
			for n := 2; n <= 44; n++ {
				rows := screen.wrap(text, n)
				requireRowsWithoutTabs(t, &screen, rows, n)
				if got, want := strings.Join(strings.Fields(strings.Join(rows, "")), ""), strings.Join(strings.Fields(text), ""); got != want {
					t.Fatalf("method %v wrap(%q, %d) shows %q, want every character of the text", method, text, n, got)
				}
			}
		}
	}
}

func wrapSweepTexts() []string {
	tokens := []string{"a", "bcd", "efghijk", "-", "--", " ", "\t", "\n", "한"}
	texts := []string{""}
	level := []string{""}
	for range 4 {
		var next []string
		for _, prefix := range level {
			for _, token := range tokens {
				next = append(next, prefix+token)
			}
		}
		texts = append(texts, next...)
		level = next
	}
	return texts
}

func wrapProblem(screen *screenState, text string, n int) string {
	rows := screen.wrap(text, n)
	if got, want := strings.Join(strings.Fields(strings.Join(rows, "")), ""), strings.Join(strings.Fields(text), ""); got != want {
		return fmt.Sprintf("%q shows %q, want every character of the text %q", rows, got, want)
	}
	for _, row := range rows {
		if strings.Contains(row, "\t") || screen.cells(row) > n {
			return fmt.Sprintf("%q holds the row %q of %d cells with a tab or past the width", rows, row, screen.cells(row))
		}
	}
	return ""
}

func TestWrapNeverMakesARowWiderThanTheWidth(t *testing.T) {
	t.Parallel()
	texts := wrapSweepTexts()
	for _, method := range []ansi.Method{ansi.WcWidth, ansi.GraphemeWidth} {
		screen := screenState{method: method}
		for n := 2; n <= 30; n++ {
			for _, text := range texts {
				if problem := wrapProblem(&screen, text, n); problem != "" {
					t.Fatalf("method %v wrap(%q, %d) = %s", method, text, n, problem)
				}
			}
		}
	}
}

func TestWrapBreaksBeforeAHyphenThatDoesNotFit(t *testing.T) {
	t.Parallel()
	cases := []struct {
		text string
		n    int
		want []string
	}{
		{"\t\t\t- item", 17, []string{"- item"}},
		{"abcdefghijklmnop \t- item", 19, []string{"abcdefghijklmnop", "- item"}},
		{" \t-", 8, []string{"-"}},
		{"abcdefghi- x", 9, []string{"abcdefghi", "- x"}},
		{"abc defghi- x", 10, []string{"abc", "defghi- x"}},
		{"abc defghi-", 10, []string{"abc", "defghi-"}},
	}
	for _, method := range []ansi.Method{ansi.WcWidth, ansi.GraphemeWidth} {
		screen := screenState{method: method}
		for _, tc := range cases {
			if got := screen.wrap(tc.text, tc.n); !slices.Equal(got, tc.want) {
				t.Errorf("method %v wrap(%q, %d) = %q, want %q", method, tc.text, tc.n, got, tc.want)
			}
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

const keycapText = "1️⃣ one 2️⃣ two 3️⃣ three 4️⃣ four 5️⃣ five 6️⃣ six 7️⃣ seven 8️⃣ eight 9️⃣ nine"

func wrapRowCases() []struct {
	name string
	text string
	n    int
	rows []string
} {
	return []struct {
		name string
		text string
		n    int
		rows []string
	}{
		{"one leading space before a long word", " " + strings.Repeat("x", 50), 20, []string{strings.Repeat("x", 20), strings.Repeat("x", 20), strings.Repeat("x", 10)}},
		{"a space between two long words after a hard break", strings.Repeat("x", 20) + " " + strings.Repeat("y", 30), 20, []string{strings.Repeat("x", 20), strings.Repeat("y", 20), strings.Repeat("y", 10)}},
		{"a line of dashes", strings.Repeat("-", 100), 39, []string{strings.Repeat("-", 39), strings.Repeat("-", 39), strings.Repeat("-", 22)}},
		{"a word before dashes", "abc " + strings.Repeat("-", 60), 20, []string{"abc", strings.Repeat("-", 20), strings.Repeat("-", 20), strings.Repeat("-", 20)}},
		{"text that fits stays", "one two three", 20, []string{"one two three"}},
		{"an indent that fits stays on the first row", "  ab " + strings.Repeat("-", 60), 20, []string{"  ab", strings.Repeat("-", 20), strings.Repeat("-", 20), strings.Repeat("-", 20)}},
		{"a newline restarts the rows", "one\nab " + strings.Repeat("-", 60), 20, []string{"one", "ab", strings.Repeat("-", 20), strings.Repeat("-", 20), strings.Repeat("-", 20)}},
		{"a row after a broken word starts without its space", "abc " + strings.Repeat("-", 60) + " zz", 20, []string{"abc", strings.Repeat("-", 20), strings.Repeat("-", 20), strings.Repeat("-", 20), "zz"}},
		{"a no-break space joins its words", strings.Repeat("x", 18) + "\u00a0yy " + strings.Repeat("-", 60), 20, []string{strings.Repeat("x", 18) + "\u00a0y", "y", strings.Repeat("-", 20), strings.Repeat("-", 20), strings.Repeat("-", 20)}},
		{"a limit below one cell leaves the text whole", "abc def", 0, []string{"abc def"}},
	}
}

func TestWrapKeepsEveryRowWithinTheCells(t *testing.T) {
	t.Parallel()
	for _, method := range []ansi.Method{ansi.WcWidth, ansi.GraphemeWidth} {
		screen := screenState{method: method}
		for _, tc := range wrapRowCases() {
			got := screen.wrap(tc.text, tc.n)
			if strings.Join(got, "|") != strings.Join(tc.rows, "|") {
				t.Errorf("method %v %s: wrap(%q, %d) = %q, want %q", method, tc.name, tc.text, tc.n, got, tc.rows)
			}
		}
	}
}

func TestWrapRowsOfKeycapsFitTheCellsTheRendererDraws(t *testing.T) {
	t.Parallel()
	screen := screenState{method: ansi.GraphemeWidth}
	texts := []string{keycapText, strings.Repeat("x1️⃣", 40), strings.Repeat("1️⃣", 10) + " tail text", strings.Repeat("1️⃣", 30)}
	for _, text := range texts {
		for _, n := range []int{9, 19, 39} {
			rows := screen.wrap(text, n)
			for _, row := range rows {
				if got := ansi.StringWidth(row); got > n {
					t.Errorf("wrap(%q, %d) has the row %q of %d cells", text, n, row, got)
				}
			}
			if kept, want := removeSpaces(strings.Join(rows, "")), removeSpaces(text); kept != want {
				t.Errorf("wrap(%q, %d) lost text: %q", text, n, kept)
			}
		}
	}
}

func removeSpaces(text string) string {
	return strings.Map(func(r rune) rune {
		if unicode.IsSpace(r) {
			return -1
		}
		return r
	}, text)
}

func TestWrapKeepsAKeycapWhole(t *testing.T) {
	t.Parallel()
	const keycap = "1\uFE0F\u20E3"
	for _, tc := range []struct {
		method ansi.Method
		rows   []string
	}{
		{ansi.WcWidth, []string{"xxxxxxxx" + keycap}},
		{ansi.GraphemeWidth, []string{"xxxxxxxx", keycap}},
	} {
		screen := screenState{method: tc.method}
		if got := screen.wrap("xxxxxxxx"+keycap, 9); strings.Join(got, "|") != strings.Join(tc.rows, "|") {
			t.Errorf("method %v: wrap = %q, want %q", tc.method, got, tc.rows)
		}
	}
}

func TestRowStartSkipsOnlyWholeSpaceClusters(t *testing.T) {
	t.Parallel()
	screen := screenState{method: ansi.WcWidth}
	for _, tc := range []struct {
		flat, part string
		pos, want  int
	}{
		{"ab cd", "cd", 0, 0},
		{"ab cd", "cd", 2, 3},
		{"ab ́cd", "cd", 2, 5},
		{"ab　́cd", "cd", 2, 7},
		{"ab xcd", "cd", 2, 2},
	} {
		if got := screen.rowStart(tc.flat, tc.pos, tc.part); got != tc.want {
			t.Errorf("rowStart(%q, %d, %q) = %d, want %d", tc.flat, tc.pos, tc.part, got, tc.want)
		}
	}
}
