package tui

import (
	"fmt"
	"strings"
	"testing"
	"unicode"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"golang.org/x/text/unicode/norm"
)

const spacedSentence = "the quick brown fox jumps over the lazy dog again and again and again until the end of the line x"

type wrapCase struct {
	name        string
	width       int
	height      int
	text        string
	caret       int
	rows        []string
	x, y        int
	unicodeCore bool
}

func composerState(t *testing.T, width, height int, text string, caret int) model {
	t.Helper()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: width, Height: height})
	m.input.composer.SetValue(text)
	if caret >= 0 {
		m.input.composer.cursor = caret
	}
	return m
}

func composerStateFor(t *testing.T, tc wrapCase) model {
	t.Helper()
	m := composerState(t, tc.width, tc.height, tc.text, tc.caret)
	if tc.unicodeCore {
		m, _ = drive(t, m, tea.ModeReportMsg{Mode: ansi.ModeUnicodeCore, Value: ansi.ModeSet})
	}
	return m
}

func visibleComposerRows(m model, from int) []string {
	lines := strings.Split(stripANSI(m.View().Content), "\n")
	count := len(m.dockLines()) - 2
	rows := make([]string, 0, count)
	for i := range count {
		lead := strings.Repeat(" ", promptCells)
		if from+i == 0 {
			lead = promptText
		}
		rows = append(rows, strings.TrimPrefix(lines[i], lead))
	}
	return rows
}

func TestComposerWrapsEveryRowAtWindowWidthMinusThree(t *testing.T) {
	t.Parallel()
	const hangulLine = "가나다라마바사아자차카타파하거너더러머버"
	cases := []wrapCase{
		{
			name: "spaced sentence over three rows at 40 columns", width: 40, height: 12, text: spacedSentence, caret: -1,
			rows: []string{"the quick brown fox jumps over the", "lazy dog again and again and again", "until the end of the line x"},
			x:    promptCells + 27, y: 2,
		},
		{
			name: "spaced sentence over seven rows at 20 columns", width: 20, height: 12, text: spacedSentence, caret: -1,
			rows: []string{"the quick brown", "fox jumps over", "the lazy dog", "again and again", "and again until", "the end of the", "line x"},
			x:    promptCells + 6, y: 6,
		},
		{
			name: "hangul words over four rows at 20 columns", width: 20, height: 12, text: "나는 오늘 책을 읽고 밥을 먹고 물을 마셨 집에 갔다", caret: -1,
			rows: []string{"나는 오늘 책을", "읽고 밥을 먹고", "물을 마셨 집에", "갔다"},
			x:    promptCells + 4, y: 3,
		},
		{
			name: "hangul without spaces breaks after eight syllables", width: 20, height: 12, text: hangulLine, caret: -1,
			rows: []string{"가나다라마바사아", "자차카타파하거너", "더러머버"},
			x:    promptCells + 8, y: 2,
		},
		{
			name: "caret at the end of a full hangul row", width: 20, height: 12, text: hangulLine, caret: 8,
			rows: []string{"가나다라마바사아", "자차카타파하거너", "더러머버"},
			x:    promptCells + 16, y: 0,
		},
		{
			name: "caret after the first syllable of the second hangul row", width: 20, height: 12, text: hangulLine, caret: 9,
			rows: []string{"가나다라마바사아", "자차카타파하거너", "더러머버"},
			x:    promptCells + 2, y: 1,
		},
		{
			name: "no spaces over three rows", width: 40, height: 12, text: strings.Repeat("a", 100), caret: -1,
			rows: []string{strings.Repeat("a", 37), strings.Repeat("a", 37), strings.Repeat("a", 26)},
			x:    promptCells + 26, y: 2,
		},
		{
			name: "caret inside the third row of an unbroken word", width: 40, height: 12, text: strings.Repeat("a", 100), caret: 80,
			rows: []string{strings.Repeat("a", 37), strings.Repeat("a", 37), strings.Repeat("a", 26)},
			x:    promptCells + 6, y: 2,
		},
		{
			name: "five spaces at the break are dropped", width: 40, height: 12, text: strings.Repeat("x", 30) + "     " + strings.Repeat("y", 10), caret: -1,
			rows: []string{strings.Repeat("x", 30), strings.Repeat("y", 10)},
			x:    promptCells + 10, y: 1,
		},
		{
			name: "caret before the dropped spaces", width: 40, height: 12, text: strings.Repeat("x", 30) + "     " + strings.Repeat("y", 10), caret: 30,
			rows: []string{strings.Repeat("x", 30), strings.Repeat("y", 10)},
			x:    promptCells + 30, y: 0,
		},
		{
			name: "caret after the dropped spaces", width: 40, height: 12, text: strings.Repeat("x", 30) + "     " + strings.Repeat("y", 10), caret: 35,
			rows: []string{strings.Repeat("x", 30), strings.Repeat("y", 10)},
			x:    promptCells, y: 1,
		},
		{
			name: "a line that ends exactly at the width stays on one row", width: 40, height: 12, text: strings.Repeat("a", 37), caret: -1,
			rows: []string{strings.Repeat("a", 37)},
			x:    promptCells + 37, y: 0,
		},
		{
			name: "a space after a full row opens no blank row", width: 40, height: 12, text: strings.Repeat("a", 37) + " ", caret: -1,
			rows: []string{strings.Repeat("a", 37)},
			x:    promptCells + 37, y: 0,
		},
		{
			name: "a word after a full row starts the second row", width: 40, height: 12, text: strings.Repeat("a", 37) + " b", caret: -1,
			rows: []string{strings.Repeat("a", 37), "b"},
			x:    promptCells + 1, y: 1,
		},
		{
			name: "caret in the middle of the second row", width: 40, height: 12, text: spacedSentence, caret: 39,
			rows: []string{"the quick brown fox jumps over the", "lazy dog again and again and again", "until the end of the line x"},
			x:    promptCells + 4, y: 1,
		},
		{
			name: "caret at the start of the third row", width: 40, height: 12, text: spacedSentence, caret: 70,
			rows: []string{"the quick brown fox jumps over the", "lazy dog again and again and again", "until the end of the line x"},
			x:    promptCells, y: 2,
		},
		{
			name: "a second line wraps below the first", width: 40, height: 12, text: "ab\n" + spacedSentence, caret: -1,
			rows: []string{"ab", "the quick brown fox jumps over the", "lazy dog again and again and again", "until the end of the line x"},
			x:    promptCells + 27, y: 3,
		},
		{
			name: "decomposed words wrap as composed", width: 20, height: 12, text: strings.TrimSpace(strings.Repeat("café ", 7)), caret: -1,
			rows: []string{"café café café", "café café café", "café"},
			x:    promptCells + 4, y: 2,
		},
		{
			name: "a tab before the break keeps its spaces and the second row", width: 40, height: 12, text: "ab\tcdefghijklmno pqrstuvwxyz abcdefghijklmnop", caret: -1,
			rows: []string{"ab      cdefghijklmno pqrstuvwxyz", "abcdefghijklmnop"},
			x:    promptCells + 16, y: 1,
		},
		{
			name: "a tab on the first row of a narrow window", width: 20, height: 12, text: "a\tbcdefgh ijklmnopqrstu", caret: 2,
			rows: []string{"a       bcdefgh", "ijklmnopqrstu"},
			x:    promptCells + 8, y: 0,
		},
		{
			name: "a tab on the second row keeps the second row inside the window", width: 20, height: 12, text: "abcdefghijklmnopq ab\t12345678901", caret: -1,
			rows: []string{"abcdefghijklmnopq", "ab    12345678901"},
			x:    promptCells + 17, y: 1,
		},
		{
			name: "the caret after a tab on the second row sits on the next character", width: 20, height: 12, text: "abcdefghijklmnopq ab\t12345678901", caret: 21,
			rows: []string{"abcdefghijklmnopq", "ab    12345678901"},
			x:    promptCells + 6, y: 1,
		},
		{
			name: "a tab past the break leaves the caret at the start of the next row", width: 20, height: 12, text: "abcdefghijklmnop\tx", caret: 17,
			rows: []string{"abcdefghijklmnop", "x"},
			x:    promptCells, y: 1,
		},
		{
			name: "the caret before a tab at the break stays on the first row", width: 20, height: 12, text: "abcdefghijklmnop\tx", caret: 16,
			rows: []string{"abcdefghijklmnop", "x"},
			x:    promptCells + 16, y: 0,
		},
		{
			name: "two tabs on the second row count their stops through the wrapped row", width: 40, height: 12, text: "the quick brown fox jumps over the lazy dog\tand\tthen", caret: -1,
			rows: []string{"the quick brown fox jumps over the", "lazy dog     and     then"},
			x:    promptCells + 25, y: 1,
		},
		{
			name: "the caret after the first tab of the second row", width: 40, height: 12, text: "the quick brown fox jumps over the lazy dog\tand\tthen", caret: 44,
			rows: []string{"the quick brown fox jumps over the", "lazy dog     and     then"},
			x:    promptCells + 13, y: 1,
		},
		{
			name: "a tab after the last word of a full row moves the next word down", width: 40, height: 12, text: strings.Repeat("a", 30) + " ab\t" + "1234567890123456789012", caret: 34,
			rows: []string{strings.Repeat("a", 30) + " ab", "1234567890123456789012"},
			x:    promptCells, y: 1,
		},
		{
			name: "tabs before a hyphen leave an empty first row", width: 20, height: 12, text: "\t\t\t- item", caret: -1,
			rows: []string{"", "- item"},
			x:    promptCells + 6, y: 1,
		},
		{
			name: "the caret after three tabs sits on the hyphen", width: 20, height: 12, text: "\t\t\t- item", caret: 3,
			rows: []string{"", "- item"},
			x:    promptCells, y: 1,
		},
		{
			name: "a tab after a word moves the hyphen to the second row", width: 20, height: 12, text: "abcdefghijklmnop \t- item", caret: -1,
			rows: []string{"abcdefghijklmnop", "- item"},
			x:    promptCells + 6, y: 1,
		},
		{
			name: "the caret before that hyphen sits on it", width: 20, height: 12, text: "abcdefghijklmnop \t- item", caret: 18,
			rows: []string{"abcdefghijklmnop", "- item"},
			x:    promptCells, y: 1,
		},
		{
			name: "a tab and a space before a hyphen at 40 columns", width: 40, height: 12, text: strings.Repeat("a", 34) + " \t- item", caret: -1,
			rows: []string{strings.Repeat("a", 34), "- item"},
			x:    promptCells + 6, y: 1,
		},
		{
			name: "a hyphen after a full row opens the second row", width: 12, height: 12, text: "abcdefghi- x", caret: -1,
			rows: []string{"abcdefghi", "- x"},
			x:    promptCells + 3, y: 1,
		},
		{
			name: "a hyphen past the edge takes its word down", width: 13, height: 12, text: "abc defghi- x", caret: -1,
			rows: []string{"abc", "defghi- x"},
			x:    promptCells + 9, y: 1,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			checkWrapCase(t, tc)
		})
	}
}

func checkWrapCase(t *testing.T, tc wrapCase) {
	t.Helper()
	m := composerStateFor(t, tc)
	lines, row, col := m.input.composer.layout(&m.screen, tc.width, nil, m.screen.dockTheme())
	got := make([]string, len(lines))
	for i, line := range lines {
		got[i] = stripANSI(line)
	}
	if strings.Join(got, "|") != strings.Join(tc.rows, "|") {
		t.Fatalf("layout rows = %q, want %q", got, tc.rows)
	}
	if row != tc.y || col != tc.x-promptCells {
		t.Fatalf("layout caret at row %d column %d, want row %d column %d", row, col, tc.y, tc.x-promptCells)
	}
	from := max(0, tc.y-composerMaxRows+1)
	view := m.View()
	assertFrame(t, strings.Split(view.Content, "\n"), tc.width, tc.height)
	if view.Cursor == nil || view.Cursor.X != tc.x || view.Cursor.Y != tc.y-from {
		t.Fatalf("view cursor = %v, want (%d, %d)", view.Cursor, tc.x, tc.y-from)
	}
	want := tc.rows[from:min(len(tc.rows), from+composerMaxRows)]
	if shown := visibleComposerRows(m, from); strings.Join(shown, "|") != strings.Join(want, "|") {
		t.Fatalf("composer shows %q, want %q", shown, want)
	}
}

func TestComposerCapsAtSixRowsAndKeepsTheCaretVisible(t *testing.T) {
	t.Parallel()
	all := []string{"the quick brown", "fox jumps over", "the lazy dog", "again and again", "and again until", "the end of the", "line x"}
	cases := []struct {
		name  string
		caret int
		from  int
		y     int
		x     int
	}{
		{name: "caret at the end scrolls the first row away", caret: -1, from: 1, y: 5, x: promptCells + 6},
		{name: "caret on the first row keeps the first six rows", caret: 3, from: 0, y: 0, x: promptCells + 3},
		{name: "caret on the sixth row keeps the first six rows", caret: 79, from: 0, y: 5, x: promptCells + 3},
		{name: "caret on the seventh row scrolls one row", caret: 92, from: 1, y: 5, x: promptCells + 1},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := composerState(t, 20, 24, spacedSentence, tc.caret)
			view := m.View()
			assertFrame(t, strings.Split(view.Content, "\n"), 20, 24)
			want := all[tc.from : tc.from+composerMaxRows]
			if got := visibleComposerRows(m, tc.from); strings.Join(got, "|") != strings.Join(want, "|") {
				t.Fatalf("composer shows %q, want %q", got, want)
			}
			if view.Cursor == nil || view.Cursor.X != tc.x || view.Cursor.Y != tc.y {
				t.Fatalf("view cursor = %v, want (%d, %d)", view.Cursor, tc.x, tc.y)
			}
		})
	}
}

func TestComposerCursorStaysInsideShortWindows(t *testing.T) {
	t.Parallel()
	for _, size := range [][2]int{{40, 12}, {20, 6}, {20, 5}, {10, 8}} {
		for caret := -1; caret <= len(spacedSentence); caret += 7 {
			m := composerState(t, size[0], size[1], spacedSentence, caret)
			view := m.View()
			rows := len(m.dockLines()) - 2
			if view.Cursor == nil || view.Cursor.Y < 0 || view.Cursor.Y >= rows {
				t.Fatalf("%dx%d caret %d: cursor %v outside the %d composer rows", size[0], size[1], caret, view.Cursor, rows)
			}
			if view.Cursor.X >= size[0] {
				t.Fatalf("%dx%d caret %d: cursor column %d is outside the window", size[0], size[1], caret, view.Cursor.X)
			}
			shown := strings.Split(stripANSI(view.Content), "\n")[view.Cursor.Y]
			if strings.HasSuffix(shown, "...") {
				t.Fatalf("%dx%d caret %d: caret row ends with an ellipsis: %q", size[0], size[1], caret, shown)
			}
		}
	}
}

func TestComposerUpAndDownKeepTheColumnAcrossSpacedRows(t *testing.T) {
	t.Parallel()
	m := composerState(t, 40, 12, spacedSentence, 74)
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyUp})
	if m.input.composer.cursor != 39 {
		t.Fatalf("up from the third row left the caret at %d, want 39", m.input.composer.cursor)
	}
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyUp})
	if m.input.composer.cursor != 4 {
		t.Fatalf("up from the second row left the caret at %d, want 4", m.input.composer.cursor)
	}
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	if m.input.composer.cursor != 74 {
		t.Fatalf("down twice left the caret at %d, want 74", m.input.composer.cursor)
	}
}

func TestComposerKeepsEveryTypedCharacterAfterATab(t *testing.T) {
	t.Parallel()
	drafts := []string{
		"a\tbcdefgh ijklmnopqrstu",
		"abcdefghijklmnopq ab\t12345678901",
		"abcdefghijklmnop\tx",
		"the quick brown fox jumps over the lazy dog\tand\tthen",
		strings.Repeat("a", 30) + " ab\t1234567890123456789012",
		"abc\tdef ghi\tjkl mno\tpqr stu\tvwx yz\t0123456789",
		"\t\t\t- item",
		"abcdefghijklmnop \t- item",
		strings.Repeat("a", 34) + " \t- item",
	}
	for _, width := range []int{20, 40} {
		for _, draft := range drafts {
			m := composerState(t, width, 12, draft, -1)
			assertFrame(t, strings.Split(m.View().Content, "\n"), width, 12)
			shown := strings.Join(visibleComposerRows(m, 0), "")
			if got, want := strings.Join(strings.Fields(shown), ""), strings.Join(strings.Fields(draft), ""); got != want {
				t.Fatalf("width %d %q shows %q, want every character of %q", width, draft, got, want)
			}
			for at := 1; at < len(draft); at++ {
				if draft[at-1] != '\t' || draft[at] == ' ' || draft[at] == '\t' {
					continue
				}
				checkCaretOnCharacter(t, composerState(t, width, 12, draft, at), width, draft, at)
			}
		}
	}
}

func checkCaretOnCharacter(t *testing.T, m model, width int, draft string, at int) {
	t.Helper()
	view := m.View()
	if view.Cursor == nil || view.Cursor.X >= width-1 {
		t.Fatalf("width %d %q caret %d: cursor %v is outside the window", width, draft, at, view.Cursor)
	}
	row := []rune(strings.Split(stripANSI(view.Content), "\n")[view.Cursor.Y])
	if view.Cursor.X >= len(row) || row[view.Cursor.X] != rune(draft[at]) {
		t.Fatalf("width %d %q caret %d: cursor %v is not on %q in row %q", width, draft, at, view.Cursor, draft[at], string(row))
	}
}

func TestComposerUpAndDownKeepTheColumnAcrossTabbedRows(t *testing.T) {
	t.Parallel()
	const draft = "the quick brown fox jumps over the lazy dog\tand\tthen"
	m := composerState(t, 40, 12, draft, 13)
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	if m.input.composer.cursor != 44 {
		t.Fatalf("down from column 13 left the caret at %d, want 44 on the character after the first tab", m.input.composer.cursor)
	}
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyUp})
	if m.input.composer.cursor != 13 {
		t.Fatalf("up from the character after the first tab left the caret at %d, want 13", m.input.composer.cursor)
	}
}

func TestComposerRowsMatchWrapperOutput(t *testing.T) {
	t.Parallel()
	texts := []string{
		spacedSentence,
		"한글 한글 한글 한글 한글 한글 한글 한글 한글 한글",
		"가나다라마바사아자차카타파하거너더러머버서어저처커터퍼허",
		"café café café café café café café",
		strings.Repeat("x", 30) + "       " + strings.Repeat("y", 40),
		"a  -  b -- c --- dd -  eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee ff",
		"👨‍👩‍👧 family 👨‍👩‍👧 of 👨‍👩‍👧 three 👨‍👩‍👧 emoji 👨‍👩‍👧 here",
		"世界 世界 世界 世界 世界 世界 abc 世界世界世界世界世界世界世界世界",
		"  leading spaces and trailing ones  ",
		"one non breaking space words and some more words after",
		"ab\tcdefghijklmno pqrstuvwxyz abcdefghijklmnop",
		"abcdefghijklmnopq ab\t12345678901",
		"\tabcdefghijklmnopqrstuvwxyz0123456789",
		"ab\tabcdefghijklmnopqrstuvwxyz0123456789 \tx",
		"a\t\t\tb\t\tc \t\t d\t",
		"the quick brown fox jumps over the lazy dog\tand\tthen\t\tsome more words\there",
		"한\t글 한글\t한글한글한글\t한글 ab\té\tcafé\t👨‍👩‍👧\tx",
		"\t\t\t- item",
		"abcdefghijklmnop \t- item",
		"abcdefghi- x abc defghi- x see the spec- something else - - -- ---",
		"a-b-c-d-e-f-g-h-i-j-k-l-m-n-o-p-q-r-s-t-u-v-w-x-y-z- a- b- c-",
	}
	for width := 1; width <= 44; width++ {
		for _, text := range texts {
			for _, method := range []ansi.Method{ansi.WcWidth, ansi.GraphemeWidth} {
				checkRowsAgainstWrapper(t, screenState{method: method}, text, width)
			}
		}
	}
}

func checkRowsAgainstWrapper(t *testing.T, screen screenState, text string, width int) {
	t.Helper()
	editor := newEditor()
	editor.SetValue(text)
	rows := editor.rows(&screen, width)
	want := screen.wrap(text, editorCells(width))
	if len(rows) != len(want) {
		t.Fatalf("method %v width %d %q: %d rows, want %d: %+v", screen.method, width, text, len(rows), len(want), rows)
	}
	pos := 0
	for i, row := range rows {
		if problem := rowProblem(screen, text, pos, row, want[i]); problem != "" {
			t.Fatalf("method %v width %d %q: row %d %s", screen.method, width, text, i, problem)
		}
		pos = row.end
	}
	if tail := text[pos:]; strings.TrimFunc(tail, unicode.IsSpace) != "" {
		t.Fatalf("method %v width %d %q: rows stop before %q", screen.method, width, text, tail)
	}
}

func rowProblem(screen screenState, text string, pos int, row editorRow, want string) string {
	if row.start < pos || row.end < row.start || row.end > len(text) {
		return fmt.Sprintf("range [%d, %d) after %d", row.start, row.end, pos)
	}
	if gap := text[pos:row.start]; strings.TrimFunc(gap, unicode.IsSpace) != "" {
		return fmt.Sprintf("skips non-space %q", gap)
	}
	if row.text != text[row.start:row.end] {
		return fmt.Sprintf("text %q is not the source range", row.text)
	}
	if got := norm.NFC.String(screen.expandFrom(row.text, row.col)); got != want {
		return fmt.Sprintf("is %q, wrapper returned %q", got, want)
	}
	return ""
}
