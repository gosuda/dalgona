package tui

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"golang.org/x/text/width"

	"github.com/gosuda/dalgona/internal/rpc"
)

func bandModel(cwd string) model {
	m := newTestModel(nil, Options{Cwd: cwd})
	m.thread.id = "t1"
	m.thread.modelName = "gpt-test"
	m.thread.effort = "high"
	m.thread.levels = map[string][]string{"gpt-test": {"low", "medium", "high"}}
	m.thread.approvalMode = "auto"
	m.thread.sandbox = "workspace-write"
	m.thread.usage = rpc.ThreadTokenUsage{ModelContextWindow: 200000, Last: rpc.TokenUsageBreakdown{InputTokens: 50000}}
	m.queue = []string{"a", "b"}
	m.conn.product = "dalgona"
	return m
}

func bandRowOf(m model) string {
	return stripANSI(m.bandRow())
}

func TestStatusBandDropsWholeItems(t *testing.T) {
	t.Parallel()
	cases := []struct {
		width   int
		present []string
		absent  []string
	}{
		{60, []string{"▸ ws", "+  2", "⣿", "dalgona", "25%"}, nil},
		{37, []string{"+  2", "⣿", "dalgona", "25%"}, []string{"▸ ws"}},
		{28, []string{"⣿", "dalgona", "25%"}, []string{"▸ ws", "+  2"}},
		{27, []string{"dalgona", "25%"}, []string{"⣿"}},
		{25, []string{"25%", "✓", "⊡"}, []string{"dalgona"}},
		{17, []string{"gpt-...", "✓", "⊡", "25%"}, []string{"dalgona"}},
	}
	for _, tc := range cases {
		m := bandModel("/work/ws")
		m.screen.width = tc.width
		row := bandRowOf(m)
		for _, want := range tc.present {
			if !strings.Contains(row, want) {
				t.Errorf("width %d: row %q lacks %q", tc.width, row, want)
			}
		}
		for _, gone := range tc.absent {
			if strings.Contains(row, gone) {
				t.Errorf("width %d: row %q still shows %q", tc.width, row, gone)
			}
		}
		if cells := m.screen.cells(row); cells > tc.width-1 {
			t.Errorf("width %d: row is %d cells", tc.width, cells)
		}
	}
}

func TestStatusBandModelCut(t *testing.T) {
	t.Parallel()
	m := bandModel("/work/ws")
	m.thread.modelName = "openai/gpt-test-long"
	m.thread.levels = nil
	m.thread.usage = rpc.ThreadTokenUsage{}
	m.conn.product = ""
	m.screen.width = 12
	row := bandRowOf(m)
	if !strings.Contains(row, "...") || strings.Contains(row, "gpt-test-long") {
		t.Fatalf("model not cut with ...: %q", row)
	}
	if cells := m.screen.cells(row); cells > 11 {
		t.Fatalf("row %q is %d cells, want at most 11", row, cells)
	}
	m.screen.width = 21
	row = bandRowOf(m)
	name, rest, _ := strings.Cut(row, " ")
	if !strings.HasSuffix(name, "...") || m.screen.cells(name) != 16 || rest != "✓ ⊡" {
		t.Fatalf("model not cut to 16 cells ahead of approval and sandbox: %q", row)
	}
}

func TestStatusBandStatusHide(t *testing.T) {
	t.Parallel()
	m := bandModel("/work/ws")
	m.screen.width = 80
	m, _ = drive(t, m, resultMsg{op: "settings", value: &settingsReply{Settings: []settingValue{{Key: "statusHide", Value: []any{"folder"}}}}})
	row := bandRowOf(m)
	if strings.Contains(row, "▸ ws") || !strings.Contains(row, "+  2") {
		t.Fatalf("statusHide folder left row %q", row)
	}
	m = bandModel("/work/ws")
	m, _ = drive(t, m, resultMsg{op: "settings", value: &settingsReply{Settings: []settingValue{{Key: "statusHide", Value: []any{"model"}}}}})
	if m.notice != "status_hide: model cannot be hidden" {
		t.Fatalf("notice = %q", m.notice)
	}
	if !strings.Contains(bandRowOf(m), "gpt-test") {
		t.Fatalf("model left the row: %q", bandRowOf(m))
	}
}

func TestContextItemUpdatesFromUsage(t *testing.T) {
	t.Parallel()
	m := bandModel("/work/ws")
	m.thread.usage = rpc.ThreadTokenUsage{}
	m, _ = drive(t, m, notificationMsg{method: "thread/tokenUsage/updated", params: []byte(`{"threadId":"t1","turnId":"turn-1","tokenUsage":{"last":{"inputTokens":50000,"totalTokens":50000},"total":{"totalTokens":50000},"modelContextWindow":200000}}`)})
	if row := bandRowOf(m); !strings.Contains(row, "25%") {
		t.Fatalf("context not updated: %q", row)
	}
}

func TestIconsAutoSwitchesOnNerdFontReply(t *testing.T) {
	t.Parallel()
	const autoGlyph = "\U000f167a"
	m := bandModel("/work/ws")
	m.screen.width = 80
	if row := bandRowOf(m); !strings.Contains(row, "✓") || strings.Contains(row, autoGlyph) {
		t.Fatalf("no reply should give Unicode: %q", row)
	}
	m, _ = drive(t, m, tea.CapabilityMsg{Content: "kitty-query-font_family=Menlo"})
	if row := bandRowOf(m); !strings.Contains(row, "✓") {
		t.Fatalf("Menlo should keep Unicode: %q", row)
	}
	m, _ = drive(t, m, tea.CapabilityMsg{Content: "kitty-query-font_family=JetBrainsMono Nerd Font Mono"})
	if row := bandRowOf(m); !strings.Contains(row, autoGlyph) {
		t.Fatalf("Nerd Font reply should give Nerd glyphs: %q", row)
	}
}

func kittyQueryCount(cmds ...tea.Cmd) int {
	sent := 0
	for _, cmd := range cmds {
		if cmd != nil && fmt.Sprint(cmd()) == kittyFontQuery {
			sent++
		}
	}
	return sent
}

func TestKittyQueryNeedsKittyAndAutoIcons(t *testing.T) {
	t.Parallel()
	kitty := tea.EnvMsg([]string{"KITTY_WINDOW_ID=1"})
	plain := tea.EnvMsg([]string{"TERM=xterm-256color"})
	cases := []struct {
		name     string
		env      tea.EnvMsg
		icons    string
		wantSent int
	}{
		{"unicode setting sends nothing", kitty, "unicode", 0},
		{"nerd setting sends nothing", kitty, "nerd", 0},
		{"no kitty sends nothing", plain, "auto", 0},
		{"auto under kitty sends one query", kitty, "auto", 1},
	}
	for _, tc := range cases {
		setting := &settingsReply{Settings: []settingValue{{Key: "icons", Value: tc.icons}}}
		envStep := func(m *model) tea.Cmd { return m.applyEnvMsg(tc.env) }
		settingsStep := func(m *model) tea.Cmd { return m.applySettings(setting) }
		for _, order := range [][2]func(*model) tea.Cmd{{envStep, settingsStep}, {settingsStep, envStep}} {
			m := newTestModel(nil, Options{})
			first, second := order[0], order[1]
			sent := kittyQueryCount(first(&m), second(&m), first(&m), second(&m))
			if sent != tc.wantSent {
				t.Fatalf("%s: %d queries, want %d", tc.name, sent, tc.wantSent)
			}
		}
	}
}

func TestUnrelatedCapabilityKeepsNerdChoice(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.applyCapability(tea.CapabilityMsg{Content: "kitty-query-font_family=JetBrainsMono Nerd Font Mono"})
	m.applyCapability(tea.CapabilityMsg{Content: "Tc"})
	if !m.screen.kittyNerd {
		t.Fatal("an unrelated capability reply changed the Nerd Font choice")
	}
}

func TestBandFieldsKeepColumns(t *testing.T) {
	t.Parallel()
	first := bandModel("/work/ws")
	first.screen.width = 100
	first.thread.usage.Last.InputTokens = 10000
	first.queue = []string{"a"}
	second := bandModel("/work/ws")
	second.screen.width = 100
	second.thread.usage.Last.InputTokens = 200000
	second.thread.effort = "low"
	second.queue = make([]string, 12)
	col := func(m model, marker string) int {
		row := bandRowOf(m)
		before, _, found := strings.Cut(row, marker)
		if !found {
			t.Fatalf("marker %q missing from band row %q", marker, row)
		}
		return m.screen.cells(before)
	}
	markers := map[string]string{"approval": "✓", "sandbox": "⊡", "queue": "+", "context": "%", "mark": "dalgona"}
	for name, marker := range markers {
		want := col(first, marker)
		if name == "context" {
			want -= 3
		}
		got := col(second, marker)
		if name == "context" {
			got -= 3
		}
		if got != want {
			t.Errorf("%s starts at column %d, want %d", name, got, want)
		}
	}
	tight := bandModel("/work/ws")
	tight.screen.width = 38
	row := bandRowOf(tight)
	queue, _, found := strings.Cut(row, "+  2")
	if !found {
		t.Fatalf("count field missing from band row %q", row)
	}
	percent, _, found := strings.Cut(row, "%")
	if !found {
		t.Fatalf("percent missing from band row %q", row)
	}
	if got, want := tight.screen.cells(percent)-tight.screen.cells(queue), 4+1+3; got != want {
		t.Errorf("percent sits %d cells after the count field, want %d", got, want)
	}
}

func TestStatusBandFolderCutsAtSixteen(t *testing.T) {
	t.Parallel()
	m := bandModel("/work/" + strings.Repeat("x", 40))
	m.screen.width = 100
	row := bandRowOf(m)
	if !strings.Contains(row, "▸ "+strings.Repeat("x", 13)+"...") || strings.Contains(row, strings.Repeat("x", 14)) {
		t.Fatalf("folder not cut to 16 cells: %q", row)
	}
}

func TestBandGlyphsHaveNarrowWidth(t *testing.T) {
	t.Parallel()
	glyphs := []string{unicodeIcons.folder, unicodeIcons.queue, "\u203a"}
	glyphs = append(glyphs, unicodeIcons.effort[:]...)
	for _, value := range unicodeIcons.approval {
		glyphs = append(glyphs, value)
	}
	for _, value := range unicodeIcons.sandbox {
		glyphs = append(glyphs, value)
	}
	for _, glyph := range glyphs {
		for _, r := range glyph {
			switch kind := width.LookupRune(r).Kind(); kind {
			case width.Neutral, width.EastAsianNarrow, width.EastAsianHalfwidth:
			case width.EastAsianWide, width.EastAsianFullwidth, width.EastAsianAmbiguous:
				t.Errorf("glyph %q (U+%04X) has width class %v", glyph, r, kind)
			}
		}
	}
}

func TestNerdBandGlyphsCountOneCell(t *testing.T) {
	t.Parallel()
	screen := screenState{method: ansi.WcWidth}
	glyphs := append([]string{nerdIcons.folder, nerdIcons.queue, string(moonGlyphs[0]), string(moonGlyphs[moonSteps])}, nerdIcons.effort[:]...)
	for _, value := range nerdIcons.approval {
		glyphs = append(glyphs, value)
	}
	for _, value := range nerdIcons.sandbox {
		glyphs = append(glyphs, value)
	}
	for _, glyph := range glyphs {
		if got := screen.cells(glyph); got != 1 {
			t.Errorf("Nerd glyph U+%04X counts %d cells, want 1", []rune(glyph)[0], got)
		}
	}
}

func TestEffortGlyphFollowsRank(t *testing.T) {
	t.Parallel()
	m := bandModel("/work/ws")
	m.thread.levels = map[string][]string{"gpt-test": {"low", "medium", "high", "max"}}
	m.thread.effort = "max"
	if row := bandRowOf(m); !strings.Contains(row, "⣿") {
		t.Fatalf("max effort lacks the full gauge: %q", row)
	}
	m.thread.effort = "low"
	if row := bandRowOf(m); !strings.Contains(row, "⣀") {
		t.Fatalf("low effort lacks the empty gauge: %q", row)
	}
}

func TestEffortItemNeedsTwoLevels(t *testing.T) {
	t.Parallel()
	m := bandModel("/work/ws")
	m.thread.levels = map[string][]string{"gpt-test": {"high"}}
	if row := bandRowOf(m); strings.Contains(row, "⣿") {
		t.Fatalf("one level shows an effort item: %q", row)
	}
	m.thread.levels = map[string][]string{"gpt-test": {"low", "medium", "xhigh"}}
	if row := bandRowOf(m); strings.Contains(row, "⣿") {
		t.Fatalf("effort outside the levels shows an item: %q", row)
	}
}

func TestInitializeSetsProductForStatusBand(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{Cwd: "/work/ws"})
	m, _ = drive(t, m, resultMsg{op: "initialize", value: &rpc.InitializeResult{UserAgent: "dalgona/1.2.3", PlatformFamily: "unix"}})
	if m.conn.product != "dalgona" || m.conn.version != "1.2.3" || m.conn.platformFamily != "unix" {
		t.Fatalf("product %q version %q platform %q", m.conn.product, m.conn.version, m.conn.platformFamily)
	}
	if !strings.Contains(bandRowOf(m), "dalgona") {
		t.Fatalf("mark missing after initialize: %q", bandRowOf(m))
	}
}
