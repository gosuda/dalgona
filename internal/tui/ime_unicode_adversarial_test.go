package tui

import (
	"fmt"
	"strings"
	"testing"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/rpc"
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
