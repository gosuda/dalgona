package tui

import (
	"fmt"
	"image/color"
	"strings"
	"testing"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/colorprofile"
	"github.com/charmbracelet/x/ansi"
	"github.com/charmbracelet/x/vt"
)

func TestApprovalChoicesMeasureWithRendererMethod(t *testing.T) {
	t.Parallel()
	screen := screenState{method: ansi.WcWidth}
	family := "\U0001F468\u200d\U0001F469\u200d\U0001F467"
	if got, want := screen.cells(family), ansi.WcWidth.StringWidth(family); got != want {
		t.Fatalf("cells(family) = %d, want the renderer method width %d", got, want)
	}
	rows := approvalChoices(&screen, 0)
	if len(rows) != 2 {
		t.Fatalf("approval actions gave %d rows, want two", len(rows))
	}
	if got := screen.cells(rows[1]); got != ansi.WcWidth.StringWidth(rows[1]) {
		t.Fatalf("second action row measures %d, want the renderer width", got)
	}
}

const (
	emulatorBold    = 1
	emulatorReverse = 32
)

var (
	nightFill, nightInk = lipgloss.Color("#66A0C8"), lipgloss.Color("#100F0F")
	dayFill, dayInk     = lipgloss.Color("#1A4F8C"), lipgloss.Color("#FFFCF0")
)

var pointCases = []struct {
	name      string
	setting   string
	terminal  []tea.Msg
	fill, ink color.Color
}{
	{name: "auto before the background reply is night blue", setting: "auto", fill: nightFill, ink: nightInk},
	{name: "auto-soft before the background reply is soft night blue", setting: "auto-soft", fill: nightFill, ink: nightInk},
	{name: "explicit day is day blue", setting: "day", fill: dayFill, ink: dayInk},
	{name: "auto after a light reply is day blue", setting: "auto", terminal: []tea.Msg{tea.BackgroundColorMsg{Color: color.White}}, fill: dayFill, ink: dayInk},
	{name: "NO_COLOR before the background reply is bold reverse", setting: "auto", terminal: []tea.Msg{
		tea.EnvMsg([]string{"NO_COLOR=1", "TERM=xterm-256color"}), tea.ColorProfileMsg{Profile: colorprofile.TrueColor},
	}},
	{name: "NO_COLOR after a dark reply is bold reverse", setting: "auto", terminal: []tea.Msg{
		tea.EnvMsg([]string{"NO_COLOR=1", "TERM=xterm-256color"}), tea.ColorProfileMsg{Profile: colorprofile.TrueColor}, tea.BackgroundColorMsg{Color: color.Black},
	}},
}

var pointCards = []struct {
	name    string
	marker  string
	request serverRequestMsg
}{
	{name: "approval accept", marker: "[y] accept", request: commandApprovalRequest(make(chan requestAnswer, 1))},
	{name: "question submit", marker: "s submit", request: questionRequest(make(chan requestAnswer, 1), questionParamsJSON)},
}

type renderedStyle struct {
	bg, fg        color.Color
	bold, reverse bool
}

func renderedStyleAt(em *vt.Emulator, x, y int) renderedStyle {
	style := em.CellAt(x, y).Style
	return renderedStyle{bg: style.Bg, fg: style.Fg, bold: style.Attrs&emulatorBold != 0, reverse: style.Attrs&emulatorReverse != 0}
}

func pointCellFault(got renderedStyle, fill, ink color.Color) string {
	if want := (renderedStyle{bg: fill, fg: ink, bold: true, reverse: fill == nil}); got != want {
		return fmt.Sprintf("point style %+v, want %+v", got, want)
	}
	return ""
}

func outsideCellFault(got renderedStyle, fill color.Color) string {
	if got.bg != nil || got.reverse || fill == nil && got.fg != nil {
		return fmt.Sprintf("style %+v outside the point has a background, reverse video, or colour without colour", got)
	}
	return ""
}

func markerPosition(t *testing.T, em *vt.Emulator, marker string) (top, left int) {
	t.Helper()
	for y, line := range strings.Split(em.String(), "\n") {
		if before, _, found := strings.Cut(line, marker); found {
			return y, utf8.RuneCountInString(before)
		}
	}
	t.Fatalf("screen has no %q:\n%s", marker, em.String())
	return 0, 0
}

func assertPoint(t *testing.T, view, marker string, fill, ink color.Color) {
	t.Helper()
	em := vt.NewEmulator(80, 24)
	if _, err := em.WriteString(strings.ReplaceAll(view, "\n", "\r\n")); err != nil {
		t.Fatal(err)
	}
	top, left := markerPosition(t, em, marker)
	width := utf8.RuneCountInString(marker)
	for y := range em.Height() {
		for x := range em.Width() {
			got := renderedStyleAt(em, x, y)
			fault := outsideCellFault(got, fill)
			if y == top && x >= left && x < left+width {
				fault = pointCellFault(got, fill, ink)
			}
			if fault != "" {
				t.Fatalf("%q cell %d,%d: %s", marker, x, y, fault)
			}
		}
	}
}

func TestDecisionPointFollowsDockTheme(t *testing.T) {
	t.Parallel()
	for _, card := range pointCards {
		t.Run(card.name, func(t *testing.T) {
			t.Parallel()
			for _, tc := range pointCases {
				t.Run(tc.name, func(t *testing.T) {
					t.Parallel()
					m := newTestModel(nil, Options{})
					m.screen.themeSetting = tc.setting
					m.screen.setTheme()
					for _, msg := range tc.terminal {
						m, _ = drive(t, m, msg)
					}
					m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
					m, _ = drive(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)})
					m, _ = drive(t, m, card.request)
					assertPoint(t, m.View().Content, card.marker, tc.fill, tc.ink)
				})
			}
		})
	}
}

const featuresQuestionJSON = `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
	`{"id":"q1","header":"Features","question":"Which features?","isOther":true,"isSecret":false,"multiSelect":true,` +
	`"options":[{"label":"Search","description":"Full text."},{"label":"Export","description":"CSV."},{"label":"Sync","description":"Two-way."}]}]}`

func questionViewAt(t *testing.T, data string, width, height int) string {
	t.Helper()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: width, Height: height})
	m, _ = drive(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)})
	m, _ = drive(t, m, questionRequest(make(chan requestAnswer, 1), data))
	return m.View().Content
}

func submitRow(screen string) string {
	for line := range strings.SplitSeq(screen, "\n") {
		if strings.Contains(line, "s submit") {
			return line
		}
	}
	return screen
}

func TestQuestionActionsStayVisibleAcrossWidths(t *testing.T) {
	t.Parallel()
	navigation := []string{"↑/↓ move", "1-7 pick", "Space toggle", "Enter choose", "Tab next"}
	cases := []struct {
		name                string
		data, title, option string
		width, height       int
		keys, row           []string
	}{
		{name: "40 columns shows every hint", data: questionParamsJSON, title: "Question 1 of 2 · Database", option: "1. SQLite", width: 40, height: 12, keys: navigation},
		{name: "100 columns keeps one footer row", data: questionParamsJSON, title: "Question 1 of 2 · Database", option: "1. SQLite", width: 100, height: 24, keys: navigation, row: navigation},
		{name: "40 columns in a short window keeps the choosing keys", data: questionParamsJSON, title: "Question 1 of 2 · Database", option: "1. SQLite", width: 40, height: 8, keys: []string{"↑/↓ move", "Enter choose"}},
		{name: "40 columns in a short window keeps Space for multi-select", data: featuresQuestionJSON, title: "Question 1 of 1 · Features", option: "1. Search", width: 40, height: 8, keys: []string{"↑/↓ move", "Space toggle"}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			view := questionViewAt(t, tc.data, tc.width, tc.height)
			assertFrame(t, strings.Split(view, "\n"), tc.width, tc.height)
			screen := ansi.Strip(view)
			assertContains(t, screen, tc.title, tc.option)
			assertContains(t, screen, tc.keys...)
			row := submitRow(screen)
			assertContains(t, row, "s submit · Esc dismiss")
			assertContains(t, row, tc.row...)
			assertPoint(t, view, "s submit", nightFill, nightInk)
		})
	}
}
