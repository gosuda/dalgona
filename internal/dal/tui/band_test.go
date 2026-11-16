package tui

import (
	"fmt"
	"math"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"golang.org/x/text/width"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func bandModel(cwd string) model {
	m := newTestModel(nil, Options{Cwd: cwd})
	m.thread.id = "t1"
	m.thread.cwd = cwd
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

type contextStep struct {
	name   string
	note   string
	want   string
	absent string
	stored func(*testing.T, rpc.ThreadTokenUsage)
}

func TestContextItemUpdatesFromUsage(t *testing.T) {
	t.Parallel()
	m := bandModel("/work/ws")
	m.thread.usage = rpc.ThreadTokenUsage{}
	for _, step := range []contextStep{
		{
			name: "first usage sets the share",
			note: `{"threadId":"t1","turnId":"turn-1","tokenUsage":{"last":{"inputTokens":50000,"totalTokens":50000},"total":{"totalTokens":50000},"modelContextWindow":200000}}`,
			want: "25%",
		},
		{
			name:   "estimate wins over stale last",
			note:   `{"threadId":"t1","turnId":"turn-2","tokenUsage":{"last":{"inputTokens":7032,"totalTokens":7032},"total":{"totalTokens":7032},"modelContextWindow":32000,"estimatedContextTokens":2179}}`,
			want:   contextShare(2179, 32000),
			absent: "22%",
			stored: assertEstimateStored,
		},
		{
			name:   "zero estimate wins too",
			note:   `{"threadId":"t1","turnId":"turn-3","tokenUsage":{"last":{"inputTokens":7032,"totalTokens":7032},"total":{"totalTokens":7032},"modelContextWindow":32000,"estimatedContextTokens":0}}`,
			want:   contextShare(0, 32000),
			absent: "22%",
		},
		{
			name:   "provider update restores the last sum",
			note:   `{"threadId":"t1","turnId":"turn-4","tokenUsage":{"last":{"inputTokens":10000,"cachedInputTokens":2000,"outputTokens":4000,"totalTokens":16000},"total":{"inputTokens":10000,"cachedInputTokens":2000,"outputTokens":4000,"totalTokens":16000},"modelContextWindow":32000}}`,
			want:   contextShare(16000, 32000),
			stored: assertEstimateCleared,
		},
	} {
		m, _ = drive(t, m, notificationMsg{method: "thread/tokenUsage/updated", params: []byte(step.note)})
		row := bandRowOf(m)
		if !strings.Contains(row, step.want) {
			t.Fatalf("%s: share %q missing from row %q", step.name, step.want, row)
		}
		if step.absent != "" && strings.Contains(row, step.absent) {
			t.Fatalf("%s: stale share %q survived in row %q", step.name, step.absent, row)
		}
		if step.stored != nil {
			step.stored(t, m.thread.usage)
		}
	}
}

func contextShare(used, window int) string {
	return fmt.Sprintf("%3d%%", int(math.Round(100*float64(used)/float64(window))))
}

func assertEstimateStored(t *testing.T, usage rpc.ThreadTokenUsage) {
	t.Helper()
	if usage.Last.InputTokens != 7032 || usage.Last.TotalTokens != 7032 || usage.Total.TotalTokens != 7032 {
		t.Fatalf("provider fields overwritten: %+v", usage)
	}
	if estimate := usage.EstimatedContextTokens; estimate == nil || *estimate != 2179 {
		t.Fatalf("estimate not stored: %+v", estimate)
	}
}

func assertEstimateCleared(t *testing.T, usage rpc.ThreadTokenUsage) {
	t.Helper()
	if usage.EstimatedContextTokens != nil {
		t.Fatalf("estimate not cleared: %d", *usage.EstimatedContextTokens)
	}
	if usage.Last.InputTokens != 10000 || usage.Last.CachedInputTokens != 2000 || usage.Last.OutputTokens != 4000 || usage.Last.TotalTokens != 16000 || usage.Total.TotalTokens != 16000 {
		t.Fatalf("provider fields overwritten: %+v", usage)
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
			if !narrowRune(r) {
				t.Errorf("glyph %q (U+%04X) is not narrow", glyph, r)
			}
		}
	}
	if narrowRune('\u2019') {
		t.Error("U+2019 passes the narrow-class check, so the oracle cannot fail")
	}
	if narrowRune('\u6f22') {
		t.Error("U+6F22 passes the narrow-class check, so the wide control cannot fail")
	}
}

func narrowRune(r rune) bool {
	switch width.LookupRune(r).Kind() {
	case width.Neutral, width.EastAsianNarrow, width.EastAsianHalfwidth:
		return true
	case width.EastAsianAmbiguous, width.EastAsianWide, width.EastAsianFullwidth:
		return false
	default:
		return false
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

func TestEffortResumeEmptyAndStaleNotifications(t *testing.T) {
	t.Parallel()
	for _, effort := range []string{"max", ""} {
		m := bandModel("/work")
		m.thread.levels["gpt-test"] = []string{"low", "medium", "high", "max"}
		m.applyResumed(resumedWithTurnEffort(effort))
		want := resumedEffortGlyph(effort)
		if got := m.effortText(unicodeIcons); got != want {
			t.Fatalf("resumed effort %q glyph = %q, want %q", effort, got, want)
		}
		staleEffortsKeepGlyph(t, &m, want)
		m.setThread("other")
		if m.thread.turnEffort != nil {
			t.Fatal("thread switch retained resolved effort")
		}
	}
	m := bandModel("/work")
	m.thread.turnID = "unresolved"
	if got := m.effortText(unicodeIcons); got != "" {
		t.Fatalf("unresolved active turn claimed a glyph: %q", got)
	}
}

func resumedWithTurnEffort(effort string) *resumedThread {
	return &resumedThread{
		ThreadReadResponse: rpc.ThreadReadResponse{Thread: rpc.Thread{ID: "t1", Turns: []rpc.Turn{{ID: "active", Status: "inProgress"}}}},
		effort:             "low", turnEffort: &effort, model: "gpt-test",
	}
}

func resumedEffortGlyph(effort string) string {
	if effort == "max" {
		return "⣿"
	}
	return ""
}

func staleEffortsKeepGlyph(t *testing.T, m *model, want string) {
	t.Helper()
	for _, raw := range []string{
		`{"threadId":"other","turnId":"active","effort":"high"}`,
		`{"threadId":"t1","turnId":"stale","effort":"high"}`,
	} {
		if err := m.applyTurnEffort([]byte(raw)); err != nil {
			t.Fatal(err)
		}
		if got := m.effortText(unicodeIcons); got != want {
			t.Fatalf("stale effort changed glyph to %q, want %q", got, want)
		}
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

func costModel(micros int64, partial bool) model {
	m := bandModel("/work/ws")
	m.screen.width = 100
	m.thread.usage.CostUsdMicros = &micros
	m.thread.usage.CostPartial = partial
	return m
}

func TestFormatCost(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		micros  int64
		partial bool
		want    string
	}{
		{0, false, "$0.00"},
		{4999, false, "$0.00"},
		{5000, false, "$0.01"},
		{11475, false, "$0.01"},
		{12345000, false, "$12.35"},
		{12345000, true, "$12.35+"},
		{99994999, false, "$99.99"},
		{99994999, true, "$99.99+"},
		{99995000, false, "$100"},
		{100000000, false, "$100"},
		{100000000, true, "$100+"},
		{9999994999, false, "$9999"},
		{9999995000, false, "$9999+"},
		{math.MaxInt64, false, "$9999+"},
		{math.MaxInt64, true, "$9999+"},
	} {
		got := formatCost(tc.micros, tc.partial)
		if got != tc.want {
			t.Errorf("formatCost(%d, %v) = %q, want %q", tc.micros, tc.partial, got, tc.want)
		}
		if len(got) > costCells {
			t.Errorf("formatCost(%d, %v) = %q is wider than %d cells", tc.micros, tc.partial, got, costCells)
		}
	}
}

func TestBandShowsCostWhenKnown(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		micros  int64
		partial bool
		want    string
	}{
		{11475, false, "$0.01"},
		{12345000, false, "$12.35"},
		{12345000, true, "$12.35+"},
		{0, false, "$0.00"},
	} {
		row := bandRowOf(costModel(tc.micros, tc.partial))
		if !strings.Contains(row, tc.want) {
			t.Errorf("cost %d partial %v: row %q lacks %q", tc.micros, tc.partial, row, tc.want)
		}
		if !tc.partial && strings.Contains(row, tc.want+"+") {
			t.Errorf("cost %d: row %q marks a complete cost as partial", tc.micros, row)
		}
	}
	row := bandRowOf(costModel(12345000, false))
	last := -1
	for _, marker := range []string{"+  2", "$12.35", "25%", "dalgona"} {
		at := strings.Index(row, marker)
		if at <= last {
			t.Fatalf("marker %q sits at byte %d in row %q, want queue, cost, context, mark in that order", marker, at, row)
		}
		last = at
	}
}

func TestBandHidesCostWhenUnknown(t *testing.T) {
	t.Parallel()
	m := bandModel("/work/ws")
	m.screen.width = 100
	if row := bandRowOf(m); strings.Contains(row, "$") {
		t.Fatalf("a thread with no priced message shows a cost: %q", row)
	}
	negative := int64(-1)
	m.thread.usage.CostUsdMicros = &negative
	if row := bandRowOf(m); strings.Contains(row, "$") {
		t.Fatalf("a negative cost is shown: %q", row)
	}
	shown := costModel(12345000, false)
	if row := bandRowOf(shown); !strings.Contains(row, "$12.35") {
		t.Fatalf("a priced thread shows no cost: %q", row)
	}
	hidden, _ := drive(t, shown, resultMsg{op: "settings", value: &settingsReply{Settings: []settingValue{{Key: "statusHide", Value: []any{"cost"}}}}})
	if row := bandRowOf(hidden); strings.Contains(row, "$") || !strings.Contains(row, "25%") {
		t.Fatalf("statusHide cost left row %q", row)
	}
}

func TestCostItemKeepsSevenCells(t *testing.T) {
	t.Parallel()
	for _, micros := range []int64{1000, 12345000} {
		m := costModel(micros, false)
		_, right := m.bandSegments()
		if len(right) == 0 || right[0].id != "cost" {
			t.Fatalf("right group = %+v, want cost first", right)
		}
		if cells := m.screen.cells(right[0].text); cells != costCells {
			t.Errorf("cost %d fills %d cells, want %d", micros, cells, costCells)
		}
	}
	for cols := 20; cols <= 60; cols++ {
		short, long := costModel(1000, false), costModel(99990000, true)
		short.screen.width, long.screen.width = cols, cols
		for _, marker := range []string{"▸ ws", " +  2", "⣿", "dalgona"} {
			a, b := strings.Contains(bandRowOf(short), marker), strings.Contains(bandRowOf(long), marker)
			if a != b {
				t.Errorf("width %d: %q shown = %v for $0.00 and %v for $99.99+", cols, marker, a, b)
			}
		}
	}
}

func TestStatusBandDropsCostBetweenQueueAndEffort(t *testing.T) {
	t.Parallel()
	drops := []struct {
		item   string
		marker string
		gone   int
	}{
		{"folder", "▸ ws", 45},
		{"queue", "+  2", 40},
		{"cost", "$12.35", 35},
		{"effort", "⣿", 27},
		{"mark", "dalgona", 25},
	}
	for cols := 20; cols <= 60; cols++ {
		m := costModel(12345000, false)
		m.screen.width = cols
		row := bandRowOf(m)
		for _, drop := range drops {
			if got, want := strings.Contains(row, drop.marker), cols > drop.gone; got != want {
				t.Errorf("width %d: %s shown = %v, want %v in %q", cols, drop.item, got, want, row)
			}
		}
		for _, kept := range []string{"gpt-test", "✓", "⊡", "25%"} {
			if !strings.Contains(row, kept) {
				t.Errorf("width %d: protected %q left row %q", cols, kept, row)
			}
		}
	}
}

func TestCostItemUpdatesFromUsage(t *testing.T) {
	t.Parallel()
	m := bandModel("/work/ws")
	m.screen.width = 100
	m.thread.usage = rpc.ThreadTokenUsage{}
	m, _ = drive(t, m, notificationMsg{method: "thread/tokenUsage/updated", params: []byte(`{"threadId":"t1","turnId":"turn-1","tokenUsage":{"last":{"inputTokens":50000,"totalTokens":50000},"total":{"totalTokens":50000},"modelContextWindow":200000,"costUsdMicros":12345000,"costPartial":true}}`)})
	if row := bandRowOf(m); !strings.Contains(row, "$12.35+") {
		t.Fatalf("cost not updated: %q", row)
	}
	m, _ = drive(t, m, notificationMsg{method: "thread/tokenUsage/updated", params: []byte(`{"threadId":"t2","turnId":"turn-1","tokenUsage":{"last":{"inputTokens":50000,"totalTokens":50000},"total":{"totalTokens":50000},"modelContextWindow":200000,"costUsdMicros":99000000}}`)})
	if row := bandRowOf(m); strings.Contains(row, "$99.00") || !strings.Contains(row, "$12.35+") {
		t.Fatalf("another thread's cost changed the band: %q", row)
	}
}
