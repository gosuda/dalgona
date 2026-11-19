package tui

import (
	"fmt"
	"strings"
	"testing"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

var imeCommitTexts = []struct {
	name string
	text string
}{
	{"simplified chinese", "中文輸入測試"},
	{"traditional chinese", "繁體中文輸入測試"},
	{"korean hangul", "한국어 입력 테스트"},
	{"korean conjoining jamo", "한글 입력"},
	{"japanese kana kanji", "日本語かな漢字変換"},
	{"japanese dakuten", "がぎぐげご"},
	{"decomposed accent", "é café"},
	{"zwj family", "👨\u200d👩\u200d👧\u200d👦"},
	{"rainbow flag", "🏳️\u200d🌈"},
	{"subdivision flag", "🏴\U000e0067\U000e0062\U000e0065\U000e006e\U000e0067\U000e007f"},
	{"keycap", "1️⃣"},
	{"skin tone", "👍🏽"},
	{"regional flag", "🇰🇷"},
	{"cjk extension b", "𠀋"},
	{"fullwidth punctuation", "，。！？；：「」"},
	{"ideographic space", "　間　隔"},
}

func TestIMEKeyExtendedCommitsToEditor(t *testing.T) {
	t.Parallel()
	for _, tc := range imeCommitTexts {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := newTestModel(nil, Options{})
			m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
			m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyExtended, Text: tc.text})
			if got := m.input.composer.Value(); got != tc.text {
				t.Fatalf("composer = %q, want %q", got, tc.text)
			}
		})
	}
}

func TestIMEKeyExtendedCommitsToPickerFilter(t *testing.T) {
	t.Parallel()
	m := openModelPicker(t, nil)
	text := "한국어 입력"
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyExtended, Text: text})
	if got := m.input.field.value(); got != text {
		t.Fatalf("picker filter = %q, want %q", got, text)
	}
}

func TestIMEKeyExtendedCommitsToLoginAPIKey(t *testing.T) {
	t.Parallel()
	m := openLoginPanel(t, nil, "openai")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	text := "sk-中文🔑-かな"
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyExtended, Text: text})
	if got := m.input.field.value(); got != text {
		t.Fatalf("api key field = %q, want %q", got, text)
	}
}

func TestIMEKeyExtendedCommitsToQuestionAnswer(t *testing.T) {
	t.Parallel()
	free := `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"Name","question":"Name?","isOther":false,"isSecret":false,"multiSelect":false,"options":null}]}`
	m, _ := openQuestion(t, free)
	text := "日本語かな漢字変換"
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyExtended, Text: text})
	if got := m.input.field.value(); got != text {
		t.Fatalf("question field = %q, want %q", got, text)
	}
}

var graphemeClusters = []struct {
	name string
	text string
}{
	{"zwj family", "👨\u200d👩\u200d👧\u200d👦"},
	{"rainbow flag", "🏳️\u200d🌈"},
	{"subdivision flag", "🏴\U000e0067\U000e0062\U000e0065\U000e006e\U000e0067\U000e007f"},
	{"keycap", "1️⃣"},
	{"skin tone", "👍🏽"},
	{"regional flag", "🇰🇷"},
	{"decomposed accent", "é"},
	{"nfd jamo", "한"},
	{"nfd dakuten", "が"},
}

func TestEditorGraphemeClusterEditing(t *testing.T) {
	t.Parallel()
	for _, tc := range graphemeClusters {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := newTestModel(nil, Options{})
			m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
			m.input.composer.SetValue("x" + tc.text)
			m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyBackspace})
			if got := m.input.composer.Value(); got != "x" {
				t.Fatalf("backspace left %q, want %q", got, "x")
			}
			m.input.composer.SetValue(tc.text + "x")
			m.input.composer.cursor = 0
			m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDelete})
			if got := m.input.composer.Value(); got != "x" {
				t.Fatalf("delete left %q, want %q", got, "x")
			}
			m.input.composer.SetValue(tc.text)
			runes := utf8.RuneCountInString(tc.text)
			m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyLeft})
			if m.input.composer.cursor != 0 {
				t.Fatalf("left left cursor at %d, want 0 across %q", m.input.composer.cursor, tc.text)
			}
			m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyRight})
			if m.input.composer.cursor != runes {
				t.Fatalf("right left cursor at %d, want %d across %q", m.input.composer.cursor, runes, tc.text)
			}
		})
	}
}

func TestTextFieldGraphemeClusterEditing(t *testing.T) {
	t.Parallel()
	for _, tc := range graphemeClusters {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			assertTextFieldDeletesCluster(t, tc.text)
			assertTextFieldStepsCluster(t, tc.text)
		})
	}
}

func assertTextFieldDeletesCluster(t *testing.T, cluster string) {
	t.Helper()
	field := textField{text: []rune("x" + cluster), cursor: utf8.RuneCountInString("x" + cluster)}
	if !field.edit("backspace") || field.value() != "x" {
		t.Fatalf("backspace left %q, want %q", field.value(), "x")
	}
	field = textField{text: []rune(cluster + "x"), cursor: 0}
	if !field.edit("delete") || field.value() != "x" {
		t.Fatalf("delete left %q, want %q", field.value(), "x")
	}
}

func assertTextFieldStepsCluster(t *testing.T, cluster string) {
	t.Helper()
	end := utf8.RuneCountInString(cluster)
	field := textField{text: []rune(cluster), cursor: end}
	field.edit("left")
	if field.cursor != 0 {
		t.Fatalf("left left cursor at %d, want 0 across %q", field.cursor, cluster)
	}
	field.edit("right")
	if field.cursor != end {
		t.Fatalf("right left cursor at %d, want the cluster end across %q", field.cursor, cluster)
	}
}

func assertClusterBoundary(t *testing.T, text string, cursor int) {
	t.Helper()
	offset, runes := 0, 0
	for runes < cursor && offset < len(text) {
		cluster, _ := ansi.FirstGraphemeCluster(text[offset:], ansi.GraphemeWidth)
		if cluster == "" {
			t.Fatalf("empty grapheme cluster at byte %d of %q", offset, text)
		}
		offset += len(cluster)
		runes += utf8.RuneCountInString(cluster)
	}
	if runes != cursor {
		t.Fatalf("cursor %d splits a grapheme cluster in %q", cursor, text)
	}
}

func TestEditorRowMovementStaysOnClusterBoundaries(t *testing.T) {
	t.Parallel()
	mixed := strings.Repeat("ab👨\u200d👩\u200d👧\u200d👦1️⃣cd", 8)
	nfd := strings.Repeat("한국어각が", 8)
	text := mixed + "\n" + nfd + "\n" + strings.Repeat("xyz", 30)
	for _, unicodeCore := range []bool{false, true} {
		m := newTestModel(nil, Options{})
		m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 12})
		if unicodeCore {
			m, _ = drive(t, m, tea.ModeReportMsg{Mode: ansi.ModeUnicodeCore, Value: ansi.ModeSet})
		}
		m.input.composer.SetValue(text)
		value := m.input.composer.Value()
		for range 8 {
			m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyUp})
			assertClusterBoundary(t, value, m.input.composer.cursor)
			m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
			assertClusterBoundary(t, value, m.input.composer.cursor)
		}
	}
}

func TestEditorUpMeasuresClusterWidthInUnicodeCore(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 12})
	m, _ = drive(t, m, tea.ModeReportMsg{Mode: ansi.ModeUnicodeCore, Value: ansi.ModeSet})
	first := "👨\u200d👩\u200d👧\u200d👦ab"
	m.input.composer.SetValue(first + "\nxxxxx")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyUp})
	if want := utf8.RuneCountInString(first); m.input.composer.cursor != want {
		t.Fatalf("cursor after Up = %d, want %d at the end of %q", m.input.composer.cursor, want, first)
	}
}

func TestPasteAtByteLimitKeepsUTF8(t *testing.T) {
	t.Parallel()
	cjk := strings.Repeat("中", pasteLimit/3)
	emoji := strings.Repeat("😀", pasteLimit/4)
	truncated := strings.Repeat("a", pasteLimit-2) + "\xe4\xb8"
	cases := []struct {
		name    string
		content string
		want    string
	}{
		{"limit minus one CJK", cjk, cjk},
		{"limit exact CJK plus ascii", cjk + "a", cjk + "a"},
		{"limit exact emoji", emoji, emoji},
		{"limit plus one refused", cjk + "ab", ""},
		{"truncated sequence at limit", truncated, strings.Repeat("a", pasteLimit-2) + "\uFFFD\uFFFD"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := newTestModel(nil, Options{})
			m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
			m = rawUpdate(t, m, tea.PasteMsg{Content: tc.content})
			got := m.input.composer.Value()
			if got != tc.want {
				t.Fatalf("composer holds %d bytes, want %d", len(got), len(tc.want))
			}
			if !utf8.ValidString(got) {
				t.Fatal("composer holds invalid UTF-8")
			}
			if tc.want == "" && !strings.Contains(m.notice, "1 MiB") {
				t.Fatalf("oversize paste notice = %q, want the 1 MiB refusal", m.notice)
			}
		})
	}
}

func TestPasteChipLineBoundary(t *testing.T) {
	t.Parallel()
	lines := func(n int) string {
		parts := make([]string, n)
		for i := range parts {
			parts[i] = fmt.Sprintf("行%d😀", i)
		}
		return strings.Join(parts, "\n")
	}
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	content9 := lines(9)
	m = rawUpdate(t, m, tea.PasteMsg{Content: content9})
	if got := m.input.composer.Value(); got != content9 {
		t.Fatalf("nine-line paste = %d bytes in the composer, want inline text", len(got))
	}
	if len(m.input.composer.chips) != 0 {
		t.Fatalf("nine-line paste created %d chips, want none", len(m.input.composer.chips))
	}
	content10 := lines(10)
	m = rawUpdate(t, m, tea.PasteMsg{Content: content10})
	if len(m.input.composer.chips) != 1 {
		t.Fatalf("ten-line paste created %d chips, want one", len(m.input.composer.chips))
	}
	chip := m.input.composer.chips[0]
	if chip.text != content10 {
		t.Fatalf("chip text = %d bytes, want the ten pasted lines", len(chip.text))
	}
	if !strings.Contains(dockText(m), "[Paste #1 +10 lines]") {
		t.Fatalf("dock does not show the paste chip:\n%s", dockText(m))
	}
}

func TestSanitizeTextPreservesUnicodeStripsHostile(t *testing.T) {
	t.Parallel()
	preserved := []string{
		"中文한국어日本語",
		"👨\u200d👩\u200d👧\u200d👦",
		"🏳️\u200d🌈",
		"🏴\U000e0067\U000e0062\U000e0065\U000e006e\U000e0067\U000e007f",
		"1️⃣",
		"👍🏽",
		"🇰🇷",
		"𠀋",
		"é",
		"が",
		"한",
		"a\u200db",
		"a\u200cb",
		"，。！？",
		"　",
	}
	for _, text := range preserved {
		if got := sanitizeText(text); got != text {
			t.Errorf("sanitizeText(%q) = %q, want unchanged", text, got)
		}
	}
	stripped := []struct {
		name string
		in   string
		want string
	}{
		{"rlo", "a\u202eb", "ab"},
		{"isolates", "a\u2066b\u2069c", "abc"},
		{"lrm", "a\u200eb", "ab"},
		{"csi", "\x1b[31mred\x1b[0m", "red"},
		{"osc", "x\x1b]0;title\x07y", "xy"},
		{"pua-b", "a\U000f0001b", "ab"},
		{"pua-b high", "a\U0010fffdb", "ab"},
		{"noncharacter bmp", "a￾b", "ab"},
		{"noncharacter plane", "a\U0001fffeb", "ab"},
		{"noncharacter range", "a\ufdd0b", "ab"},
	}
	for _, tc := range stripped {
		if got := sanitizeText(tc.in); got != tc.want {
			t.Errorf("%s: sanitizeText(%q) = %q, want %q", tc.name, tc.in, got, tc.want)
		}
	}
}

func bandSegmentText(m model, id string) string {
	left, _ := m.bandSegments()
	for _, seg := range left {
		if seg.id == id {
			return seg.text
		}
	}
	return ""
}

func TestBandSanitizesModelName(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.thread.modelName = "evil\x1b]0;pwn\x07gpt"
	if got := bandSegmentText(m, "model"); got != "evilgpt" {
		t.Fatalf("model segment = %q, want %q", got, "evilgpt")
	}
	row := stripANSI(m.bandRow())
	if strings.Contains(row, "]0;") || !strings.Contains(row, "evilgpt") {
		t.Fatalf("band row lost the model name or kept the escape:\n%q", row)
	}
}

func TestBandSanitizesFolder(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.conn.opts.Cwd = "/tmp/evil\x1b[31mdir"
	got := bandSegmentText(m, "folder")
	if !strings.Contains(got, "evildir") || strings.Contains(got, "\x1b") {
		t.Fatalf("folder segment = %q, want the sanitized directory name", got)
	}
}

func TestLoginViewsSanitizeProviderID(t *testing.T) {
	t.Parallel()
	hostile := "evil\x1b]0;x\x07id"
	m := newTestModel(nil, Options{})
	m.login = &loginPanel{
		providers: []rpc.AccountProvider{{ID: hostile, Login: "apiKey"}},
		provider:  rpc.AccountProvider{ID: hostile, Login: "apiKey"},
		phase:     loginProviders,
	}
	providers := m.loginProvidersView()
	if len(providers.body) != 1 {
		t.Fatalf("providers body = %#v, want one row", providers.body)
	}
	assertSanitizedID(t, "providers row", providers.body[0])
	m.login.phase = loginMethods
	methods := m.loginPanelView()
	if len(methods.body) != 1 {
		t.Fatalf("methods body = %#v, want one row", methods.body)
	}
	assertSanitizedID(t, "methods title", methods.title)
	assertSanitizedID(t, "methods row", methods.body[0])
	m.login.phase = loginAPIKey
	assertSanitizedID(t, "api key title", m.loginPanelView().title)
	m.login.phase = loginWaiting
	assertSanitizedID(t, "waiting title", m.loginPanelView().title)
}

func assertSanitizedID(t *testing.T, where, text string) {
	t.Helper()
	if !strings.Contains(text, "evilid") || strings.Contains(text, "\x1b") || strings.Contains(text, "\x07") {
		t.Fatalf("%s = %q, want the sanitized provider id", where, text)
	}
}

func TestMarkKeywordsRejectsMidClusterSpans(t *testing.T) {
	t.Parallel()
	family := "x👨\u200d👩\u200d👧\u200d👦y"
	skin := "a👍🏽b"
	accent := "éx"
	jamo := "각x"
	dakuten := "がx"
	tests := []struct {
		name   string
		text   string
		span   keywordSpan
		marked bool
	}{
		{"family zwj boundary start", family, keywordSpan{start: 1, end: 26}, true},
		{"family zwj mid start", family, keywordSpan{start: 5, end: 27}, false},
		{"family zwj mid end", family, keywordSpan{start: 0, end: 12}, false},
		{"skin tone mid end", skin, keywordSpan{start: 0, end: 5}, false},
		{"skin tone whole cluster", skin, keywordSpan{start: 1, end: 9}, true},
		{"combining mark mid start", accent, keywordSpan{start: 1, end: 4}, false},
		{"combining cluster whole", accent, keywordSpan{start: 0, end: 3}, true},
		{"nfd jamo mid start", jamo, keywordSpan{start: 3, end: 10}, false},
		{"nfd jamo whole cluster", jamo, keywordSpan{start: 0, end: 9}, true},
		{"nfd dakuten mid start", dakuten, keywordSpan{start: 3, end: 7}, false},
		{"nfd dakuten whole cluster", dakuten, keywordSpan{start: 0, end: 6}, true},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			got := markKeywords(tc.text, []keywordSpan{tc.span}, theme{})
			if tc.marked {
				if !strings.Contains(got, "\x1b[1m") {
					t.Fatalf("markKeywords(%q, %#v) = %q, want a marked span", tc.text, tc.span, got)
				}
				return
			}
			if got != tc.text {
				t.Fatalf("markKeywords(%q, %#v) = %q, want the text unchanged", tc.text, tc.span, got)
			}
		})
	}
}

const zwjFamily = "👨\u200d👩\u200d👧\u200d👦"

var fitMethods = []ansi.Method{ansi.WcWidth, ansi.GraphemeWidth}

func TestFitMeasuresWideTextWithTheMethodOfTheRenderer(t *testing.T) {
	t.Parallel()
	texts := []string{
		"한글 텍스트 안녕하세요 세계 " + zwjFamily + " e\u0301 あいうえお中文字",
		"가나다 " + zwjFamily + zwjFamily + " 라마바",
		"flag 🇰🇷🇯🇵🇺🇸 keycap 1️⃣2️⃣3️⃣ skin 👍🏽👍🏿 rainbow 🏳️\u200d🌈 end",
		strings.Repeat(zwjFamily, 6),
		"ab" + strings.Repeat("한", 12) + "cd" + strings.Repeat("e\u0301", 8),
		"\x1b[1m한글 " + zwjFamily + " bold text\x1b[0m tail",
	}
	for _, method := range fitMethods {
		screen := screenState{method: method}
		for _, text := range texts {
			total := rendererCells(method, text)
			for n := 3; n <= total+2; n++ {
				checkFitCut(t, screen, text, n)
			}
		}
	}
}

func rendererCells(method ansi.Method, text string) int {
	if method == ansi.GraphemeWidth {
		return ansi.StringWidth(text)
	}
	return ansi.StringWidthWc(text)
}

func checkFitCut(t *testing.T, screen screenState, text string, n int) {
	t.Helper()
	got := screen.fit(text, n)
	if cells := rendererCells(screen.method, got); cells > n {
		t.Fatalf("method %v fit(%q, %d) = %q holds %d cells", screen.method, text, n, got, cells)
	}
	whole := screen.nfc(text)
	if rendererCells(screen.method, whole) <= n {
		if got != whole {
			t.Fatalf("method %v fit(%q, %d) = %q, want the text unchanged", screen.method, text, n, got)
		}
		return
	}
	plain, ok := strings.CutSuffix(ansi.Strip(got), "...")
	if !ok {
		t.Fatalf("method %v fit(%q, %d) = %q, want a cut text to end with ...", screen.method, text, n, got)
	}
	rest, ok := strings.CutPrefix(ansi.Strip(whole), plain)
	if !ok {
		t.Fatalf("method %v fit(%q, %d) = %q, want a prefix of the text", screen.method, text, n, got)
	}
	cluster, _ := ansi.FirstGraphemeCluster(rest, screen.method)
	if rendererCells(screen.method, plain+cluster) <= n-3 {
		t.Fatalf("method %v fit(%q, %d) = %q, want the cut to keep the next cluster %q", screen.method, text, n, got, cluster)
	}
}

func TestFitCountsEveryPrintableASCIIAsOneCell(t *testing.T) {
	t.Parallel()
	for _, method := range fitMethods {
		screen := screenState{method: method}
		for c := ' '; c <= '~'; c++ {
			text := strings.Repeat(string(c), 20)
			if got, want := screen.fit(text, 8), strings.Repeat(string(c), 5)+"..."; got != want {
				t.Errorf("method %v fit(%q, 8) = %q, want %q", method, text, got, want)
			}
			if got := screen.cells(string(c)); got != 1 {
				t.Errorf("method %v cells(%q) = %d, want 1", method, string(c), got)
			}
		}
	}
}

func TestFitCutsAtTheByteBoundOfALongCombiningRun(t *testing.T) {
	t.Parallel()
	for _, method := range fitMethods {
		screen := screenState{method: method}
		text := "abc" + strings.Repeat("\u0301", 50) + "d"
		if got := screen.fit(text, 3); got != "..." {
			t.Errorf("method %v fit of abc, a combining run and d in 3 cells = %q, want ...", method, got)
		}
		if got, want := screen.fit(text, 4), screen.nfc(text); got != want {
			t.Errorf("method %v fit of the same text in 4 cells = %d bytes, want the whole %d bytes", method, len(got), len(want))
		}
	}
}

func TestFitCutsInsideAWideRune(t *testing.T) {
	t.Parallel()
	cases := []struct {
		in   string
		n    int
		want string
	}{
		{"한글한글", 5, "한..."},
		{"a한글한글", 5, "a..."},
		{"ab한글한", 6, "ab..."},
		{"abc한글한", 6, "abc..."},
		{"abc한", 5, "abc한"},
		{"abcd한", 5, "ab..."},
		{"한글", 3, "..."},
		{"한글", 2, "한"},
		{"한글", 1, ""},
		{"a한", 2, "a"},
		{"日本語かな", 7, "日本..."},
		{"日本語かな", 8, "日本..."},
	}
	for _, method := range fitMethods {
		screen := screenState{method: method}
		for _, tc := range cases {
			if got := screen.fit(tc.in, tc.n); got != tc.want {
				t.Errorf("method %v fit(%q, %d) = %q, want %q", method, tc.in, tc.n, got, tc.want)
			}
		}
	}
}

func TestFitCutsClustersByTheCellsOfEachMethod(t *testing.T) {
	t.Parallel()
	family := "ab " + zwjFamily + " cdef"
	keycaps := "keycap 1️⃣2️⃣3️⃣ end"
	cases := []struct {
		method ansi.Method
		text   string
		n      int
		want   string
	}{
		{ansi.WcWidth, family, 16, family},
		{ansi.WcWidth, family, 15, "ab " + zwjFamily + " ..."},
		{ansi.WcWidth, family, 14, "ab " + zwjFamily + "..."},
		{ansi.WcWidth, family, 13, "ab ..."},
		{ansi.GraphemeWidth, family, 10, family},
		{ansi.GraphemeWidth, family, 9, "ab " + zwjFamily + " ..."},
		{ansi.GraphemeWidth, family, 8, "ab " + zwjFamily + "..."},
		{ansi.GraphemeWidth, family, 7, "ab ..."},
		{ansi.WcWidth, keycaps, 11, "keycap 1️⃣..."},
		{ansi.WcWidth, keycaps, 13, "keycap 1️⃣2️⃣3️⃣..."},
		{ansi.GraphemeWidth, keycaps, 11, "keycap ..."},
		{ansi.GraphemeWidth, keycaps, 13, "keycap 1️⃣..."},
		{ansi.GraphemeWidth, keycaps, 14, "keycap 1️⃣2️⃣..."},
	}
	for _, tc := range cases {
		screen := screenState{method: tc.method}
		if got := screen.fit(tc.text, tc.n); got != tc.want {
			t.Errorf("method %v fit(%q, %d) = %q, want %q", tc.method, tc.text, tc.n, got, tc.want)
		}
	}
}

func TestFitMatchesTheRendererTruncateWhereItMeasuresRight(t *testing.T) {
	t.Parallel()
	screen := screenState{method: ansi.GraphemeWidth}
	texts := []string{
		"plain ascii text that is cut",
		"한글 텍스트 안녕하세요 세계 " + zwjFamily + " e\u0301 あいうえお中文字",
		"\x1b[31mred\x1b[0m and \x1b[1;4mbold underline\x1b[0m text",
		"\x1b]8;;https://example.com\x07link text here\x1b]8;;\x07 after",
		"한\x1b[7m글\x1b[27m한글한글한글",
	}
	for _, text := range texts {
		for n := 3; n <= screen.cells(text)+1; n++ {
			want := ansi.Truncate(screen.nfc(text), n, "...")
			if got := screen.fit(text, n); got != want {
				t.Errorf("fit(%q, %d) = %q, want %q", text, n, got, want)
			}
		}
	}
}

func TestFitKeepsTextThatFitsAfterALongCombiningRun(t *testing.T) {
	t.Parallel()
	marks := strings.Repeat("\u0301", 120)
	for _, method := range fitMethods {
		screen := screenState{method: method}
		fits := "e" + marks + "xyz"
		if got, want := screen.fit(fits, 5), screen.nfc(fits); got != want {
			t.Errorf("method %v fit of a combining run and xyz = %d bytes, want the %d bytes of the whole text", method, len(got), len(want))
		}
		over := "e" + marks + "xyzabcdef"
		got := screen.fit(over, 5)
		if want := screen.nfc("e"+marks+"x") + "..."; got != want {
			t.Errorf("method %v fit of a combining run and xyzabcdef = %d bytes ending %q, want %d bytes ending %q", method, len(got), got[max(0, len(got)-8):], len(want), want[len(want)-8:])
		}
		if cells := screen.cells(got); cells > 5 {
			t.Errorf("method %v fit of a combining run holds %d cells, want at most 5", method, cells)
		}
	}
}

func TestComposerPlacesTheCaretAfterWideText(t *testing.T) {
	t.Parallel()
	han18 := strings.Repeat("한", 18)
	han10 := strings.Repeat("한", 10)
	cases := []wrapCase{
		{
			name: "an eighteenth syllable that would straddle the edge moves to the second row", width: 40, height: 12, text: "xx" + han18, caret: -1,
			rows: []string{"xx" + strings.Repeat("한", 17), "한"},
			x:    promptCells + 2, y: 1,
		},
		{
			name: "caret at the end of the row before a straddling syllable", width: 40, height: 12, text: "xx" + han18, caret: 19,
			rows: []string{"xx" + strings.Repeat("한", 17), "한"},
			x:    promptCells + 36, y: 0,
		},
		{
			name: "a row of ascii and syllables that ends exactly at the edge", width: 40, height: 12, text: "x" + han18, caret: -1,
			rows: []string{"x" + han18},
			x:    promptCells + 37, y: 0,
		},
		{
			name: "caret after the ninth syllable of an exact row", width: 40, height: 12, text: "x" + han18, caret: 10,
			rows: []string{"x" + han18},
			x:    promptCells + 19, y: 0,
		},
		{
			name: "two syllables past the edge at 20 columns", width: 20, height: 12, text: "ab" + han10, caret: -1,
			rows: []string{"ab" + strings.Repeat("한", 7), strings.Repeat("한", 3)},
			x:    promptCells + 6, y: 1,
		},
		{
			name: "caret at the end of the first mixed row at 20 columns", width: 20, height: 12, text: "ab" + han10, caret: 9,
			rows: []string{"ab" + strings.Repeat("한", 7), strings.Repeat("한", 3)},
			x:    promptCells + 16, y: 0,
		},
		{
			name: "caret after one syllable of the second mixed row", width: 20, height: 12, text: "ab" + han10, caret: 10,
			rows: []string{"ab" + strings.Repeat("한", 7), strings.Repeat("한", 3)},
			x:    promptCells + 2, y: 1,
		},
		{
			name: "mixed words break at the space before the syllable that does not fit", width: 20, height: 12, text: "가 a나 b다 c라 d마 e바", caret: -1,
			rows: []string{"가 a나 b다 c라", "d마 e바"},
			x:    promptCells + 7, y: 1,
		},
		{
			name: "caret after the first word of a mixed row", width: 20, height: 12, text: "가 a나 b다 c라 d마 e바", caret: 1,
			rows: []string{"가 a나 b다 c라", "d마 e바"},
			x:    promptCells + 2, y: 0,
		},
		{
			name: "family emoji counts as the sum of its parts without unicode core", width: 40, height: 12, text: "a" + zwjFamily, caret: -1,
			rows: []string{"a" + zwjFamily},
			x:    promptCells + 9, y: 0,
		},
		{
			name: "family emoji counts as one glyph with unicode core", width: 40, height: 12, text: "a" + zwjFamily, caret: -1, unicodeCore: true,
			rows: []string{"a" + zwjFamily},
			x:    promptCells + 3, y: 0,
		},
		{
			name: "caret before the family emoji", width: 40, height: 12, text: "한a" + zwjFamily, caret: 2, unicodeCore: true,
			rows: []string{"한a" + zwjFamily},
			x:    promptCells + 3, y: 0,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			checkWrapCase(t, tc)
		})
	}
}
