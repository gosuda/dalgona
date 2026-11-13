package tui

import (
	"cmp"
	"hash/fnv"
	"image/color"
	"reflect"
	"regexp"
	"strconv"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"charm.land/glamour/v2/ansi"
	"charm.land/glamour/v2/styles"
	"github.com/charmbracelet/colorprofile"
)

var sgrPattern = regexp.MustCompile(`\x1b\[([0-9;]*)m`)

func colourSGR(t *testing.T, s string) bool {
	t.Helper()
	for _, match := range sgrPattern.FindAllStringSubmatch(s, -1) {
		if sgrHasColour(t, s, match[1]) {
			return true
		}
	}
	return false
}

func sgrHasColour(t *testing.T, s, params string) bool {
	t.Helper()
	for code := range strings.SplitSeq(params, ";") {
		n, err := strconv.Atoi(cmp.Or(code, "0"))
		if err != nil {
			t.Fatalf("SGR parameter %q in %q is not a number: %v", code, s, err)
		}
		if isColourCode(n) {
			return true
		}
	}
	return false
}

func isColourCode(n int) bool {
	return (n >= 30 && n <= 38) || (n >= 40 && n <= 48) || (n >= 90 && n <= 97) || (n >= 100 && n <= 107)
}

func backgroundFor(dark bool) color.Color {
	if dark {
		return color.Black
	}
	return color.White
}

func TestNoColorFrameHasNoColourSequences(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m.screen.width = 60
	m, _ = drive(t, m, tea.EnvMsg([]string{"NO_COLOR=1", "TERM=xterm-256color"}))
	m, _ = drive(t, m, tea.ColorProfileMsg{Profile: colorprofile.TrueColor})
	m, _ = drive(t, m, tea.BackgroundColorMsg{Color: color.Black})
	m, _ = drive(t, m, notificationMsg{method: "item/started", params: []byte(`{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"agentMessage","id":"a1","text":""}}`)})
	m, _ = drive(t, m, notificationMsg{method: "item/agentMessage/delta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":"# Title\n\nSee *this* and [docs](https://example.test).\n"}`)})
	m, _ = drive(t, m, approvalRequest("make test"))
	if view := m.dockView().Content; colourSGR(t, view) {
		t.Fatalf("NO_COLOR frame holds a colour sequence:\n%q", view)
	}
	rendered := m.renderMarkdown("# Title\n\nSee *this* and [docs](https://example.test).\n")
	if colourSGR(t, rendered) || strings.Contains(rendered, "\x1b[3m") || strings.Contains(rendered, "\x1b[4m") {
		t.Fatalf("NO_COLOR markdown keeps colour, italic or underline: %q", rendered)
	}
	assertContains(t, rendered, "Title", "docs")
}

func TestThemeAutoFollowsBackground(t *testing.T) {
	t.Parallel()
	cases := []struct {
		setting string
		dark    bool
		want    themeName
	}{
		{setting: "auto", dark: true, want: themeNight},
		{setting: "auto", dark: false, want: themeDay},
		{setting: "auto-soft", dark: true, want: themeSoftNight},
		{setting: "auto-soft", dark: false, want: themeSoftDay},
		{setting: "day", dark: true, want: themeDay},
		{setting: "soft-day", dark: true, want: themeSoftDay},
	}
	for _, tc := range cases {
		m := newModel(nil, Options{}, nil)
		m.screen.themeSetting = tc.setting
		m, _ = drive(t, m, tea.BackgroundColorMsg{Color: backgroundFor(tc.dark)})
		if !m.screen.theme.known || m.screen.theme.name != tc.want {
			t.Fatalf("setting %s dark %v: theme = %+v, want %s", tc.setting, tc.dark, m.screen.theme, tc.want)
		}
	}
}

func TestThemeFrameWidths(t *testing.T) {
	t.Parallel()
	for _, name := range []string{"day", "soft-day", "night", "soft-night"} {
		for _, width := range []int{40, 100} {
			m := newTestModel(nil, Options{}, nil)
			m.screen.width = width
			m.input.composer.SetWidth(width)
			m.screen.themeSetting = name
			m.screen.setTheme()
			m.errorf("boom")
			assertFrame(t, strings.Split(m.View().Content, "\n"), width, 24)
		}
	}
}

func TestRowsBeforeThemeUseOnlyDefaultDimBold(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	before := m.screen.theme
	m.commit("row:before", []string{before.paintBold(roleFailedText, "Failed") + " " + before.paint(roleAccentText, "ultrathink") + " " + before.paint(roleAddedText, "+added line")})
	m.commit("row:markdown", []string{m.renderMarkdown("# Title\n\n*emph*, `code`, and [link](https://example.test).\n")})
	for _, entry := range m.out.entries {
		if colourSGR(t, entry.text) {
			t.Fatalf("row before the theme holds a colour: %q", entry.text)
		}
		if strings.Contains(entry.text, "\x1b[3m") || strings.Contains(entry.text, "\x1b[4m") || strings.Contains(entry.text, "\x1b[7m") {
			t.Fatalf("row before the theme holds italic, underline or reverse: %q", entry.text)
		}
	}
	if text := m.out.entries[0].text; !strings.HasPrefix(text, "\x1b[1m") {
		t.Fatalf("failed word is not bold: %q", text)
	}
}

func TestMarkdownNoColourUsesNoTTYStyle(t *testing.T) {
	t.Parallel()
	for _, th := range []theme{newTheme(themeNight, false), newTheme(themeDay, false), {}} {
		if !reflect.DeepEqual(markdownStyle(th), styles.NoTTYStyleConfig) {
			t.Fatalf("markdownStyle(%+v) is not the no-colour style", th)
		}
	}
}

func TestMarkdownHeadingsUsePaletteOnly(t *testing.T) {
	t.Parallel()
	th := newTheme(themeNight, true)
	cfg := markdownStyle(th)
	want := paletteHex(th, roleText)
	headings := []struct {
		name  string
		block ansi.StyleBlock
	}{
		{"H1", cfg.H1}, {"H2", cfg.H2}, {"H3", cfg.H3}, {"H4", cfg.H4}, {"H5", cfg.H5}, {"H6", cfg.H6},
	}
	for _, h := range headings {
		if h.block.Color == nil || *h.block.Color != want {
			got := "none"
			if h.block.Color != nil {
				got = *h.block.Color
			}
			t.Fatalf("%s colour = %s, want %s", h.name, got, want)
		}
		if h.block.BackgroundColor != nil {
			t.Fatalf("%s background = %s, want none", h.name, *h.block.BackgroundColor)
		}
	}
}

func TestPaintKeepsWeightSeparate(t *testing.T) {
	t.Parallel()
	for _, th := range []theme{newTheme(themeNight, true), newTheme(themeNight, false), {}} {
		if s := th.paint(roleFailedText, "x"); strings.Contains(s, "\x1b[1m") || strings.Contains(s, "\x1b[1;") {
			t.Fatalf("paint(roleFailedText) under %+v is bold: %q", th, s)
		}
	}
}

func TestThemeChangeKeepsWrittenRows(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 80
	m.screen.color = true
	m.screen.profile = colorprofile.TrueColor
	m.screen.themeSetting = "night"
	m.screen.setTheme()
	m.commit("k1", []string{m.screen.theme.paint(roleAccentText, "title")})
	before, ok := m.nextChunk(10)
	if !ok || !strings.Contains(before, "38;2;218;112;44") {
		t.Fatalf("night row is not printed in the night accent: %q", before)
	}
	m.screen.themeSetting = "day"
	m.updateColour(tea.BackgroundColorMsg{Color: backgroundFor(false)})
	m.commit("k2", []string{m.screen.theme.paint(roleAccentText, "title")})
	after, ok := m.nextChunk(10)
	if !ok || !strings.Contains(after, "38;2;113;50;13") {
		t.Fatalf("day row is not printed in the day accent: %q", after)
	}
	if strings.Contains(after, "38;2;218;112;44") {
		t.Fatalf("theme change printed a written row again: %q", after)
	}
}

func TestDockUsesNightUntilBackgroundReply(t *testing.T) {
	t.Parallel()
	cases := []struct {
		setting string
		want    themeName
	}{
		{setting: "auto", want: themeNight},
		{setting: "auto-soft", want: themeSoftNight},
	}
	for _, tc := range cases {
		m := newModel(nil, Options{}, nil)
		m.screen.themeSetting = tc.setting
		m.screen.setTheme()
		if m.screen.theme.known {
			t.Fatalf("setting %s is known before the background reply", tc.setting)
		}
		if got := m.screen.dockTheme(); got.name != tc.want || !got.known {
			t.Fatalf("setting %s: dock theme = %+v, want %s", tc.setting, got, tc.want)
		}
	}
}

func TestThreadHueMatchesFNV(t *testing.T) {
	t.Parallel()
	for _, id := range []string{"", "t1", "thread-42", "스레드"} {
		h := fnv.New32a()
		_, _ = h.Write([]byte(id))
		if got, want := threadHue(id), int(h.Sum32()%4); got != want {
			t.Fatalf("threadHue(%q) = %d, want %d", id, got, want)
		}
	}
}

func TestLiveTagFillAndNoColourReverse(t *testing.T) {
	t.Parallel()
	colour := newTheme(themeNight, true)
	got := colour.liveTag(1, "Working")
	for _, want := range []string{"48;2;58;169;159", "38;2;16;15;15", "Working"} {
		if !strings.Contains(got, want) {
			t.Fatalf("live tag %q lacks %q", got, want)
		}
	}
	mono := newTheme(themeNight, false)
	if got := mono.liveTag(1, "Working"); !strings.Contains(got, "\x1b[1;7m") {
		t.Fatalf("no-colour live tag is not bold reverse: %q", got)
	}
}

func TestErrorNoticeIsBoldFailedText(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.errorf("boom")
	if !strings.Contains(m.liveRow(), "\x1b[1;38;2;232;112;95mboom") {
		t.Fatalf("error notice is not bold failed text: %q", m.liveRow())
	}
	m.noticef("boom")
	if strings.Contains(m.liveRow(), "\x1b[1;38;2;232;112;95m") {
		t.Fatalf("plain notice is painted as an error: %q", m.liveRow())
	}
}
