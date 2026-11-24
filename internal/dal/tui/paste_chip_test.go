package tui

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

const chipProbePUA = "a\U000F0001b"

func TestPrivateUseTextBesideChipRoundTrips(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newTestModel(caller, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m = typeText(t, m, chipProbePUA)
	m, _ = drive(t, m, tea.PasteMsg{Content: pasteLines(12)})
	if got := m.input.composer.Value(); got != chipProbePUA {
		t.Fatalf("composer holds %q, want the typed private-use text %q", got, chipProbePUA)
	}
	m = typeText(t, m, "\U000F0002")
	want := chipProbePUA + pasteLines(12) + "\U000F0002"
	if got := m.input.composer.Expanded(); got != want {
		t.Fatalf("expanded = %q, want %q", got, want)
	}
	m.thread.id = "t1"
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	runCommand(t, m, command, "Enter did not send the prompt")
	requireLastPrompt(t, caller.calls, 1, "turn/start", want)
}

func TestPrivateUseInsidePasteChipStaysByteExact(t *testing.T) {
	t.Parallel()
	lines := make([]string, 12)
	for i := range lines {
		lines[i] = "row with \U000F0001 and \u200e marks"
	}
	pasted := strings.Join(lines, "\n")
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, tea.PasteMsg{Content: pasted})
	if len(m.input.composer.chips) != 1 {
		t.Fatalf("composer holds %d chips, want 1", len(m.input.composer.chips))
	}
	if got := m.input.composer.Expanded(); got != pasted {
		t.Fatalf("expanded = %q, want the pasted text byte-exact", got)
	}
}

func TestSingleLineFieldsRefuseMultilinePaste(t *testing.T) {
	t.Parallel()
	setup := []struct {
		name  string
		open  func(t *testing.T) model
		value func(m model) string
	}{
		{name: "filter", open: func(t *testing.T) model {
			return openModelPicker(t, new(fakeCaller))
		}, value: func(m model) string { return m.input.field.value() }},
		{name: "target", open: func(t *testing.T) model {
			m := openModelPicker(t, new(fakeCaller))
			m.list.pending = "baseBranch"
			return m
		}, value: func(m model) string { return m.input.field.value() }},
		{name: "answer", open: func(t *testing.T) model {
			free := `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
				`{"id":"q1","header":"Name","question":"Name?","isOther":false,"isSecret":false,"multiSelect":false,"options":null}]}`
			m, _ := openQuestion(t, free)
			return m
		}, value: func(m model) string { return m.input.field.value() }},
		{name: "api key", open: func(t *testing.T) model {
			m := openLoginPanel(t, new(fakeCaller), "openai")
			m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
			return m
		}, value: func(m model) string { return m.input.field.value() }},
	}
	for _, field := range setup {
		t.Run(field.name, func(t *testing.T) {
			t.Parallel()
			checkSingleLineRefusal(t, field.open, field.value)
		})
	}
}

func checkSingleLineRefusal(t *testing.T, open func(t *testing.T) model, value func(m model) string) {
	t.Helper()
	for _, in := range []string{"one\ntwo", "one\rtwo", "one\r\ntwo", "one\u000btwo", "one\u2028two"} {
		m := open(t)
		m.input.field = textField{text: []rune("prior"), cursor: 5}
		m, _ = drive(t, m, tea.PasteMsg{Content: in})
		if got := value(m); got != "prior" {
			t.Fatalf("%q: field holds %q, want the prior value kept whole", in, got)
		}
		if m.input.field.cursor != 5 {
			t.Fatalf("%q: cursor at %d, want 5", in, m.input.field.cursor)
		}
		if !strings.Contains(m.notice, "takes one line") {
			t.Fatalf("%q: notice %q, want a visible one-line refusal", in, m.notice)
		}
	}
}

func TestFieldPasteRefusalNamesTheReason(t *testing.T) {
	t.Parallel()
	prior := textField{text: []rune("prior"), cursor: 5}
	cases := []struct{ paste, want string }{
		{"one\r\ntwo\u2028three", "one line"},
		{"a\x00b", "NUL"},
		{"a\xffb", "UTF-8"},
		{strings.Repeat("x", pasteLimit), "1 MiB"},
	}
	if got := prior.pasteRefusal("one two"); got != "" {
		t.Fatalf("one-line paste refused with %q", got)
	}
	for _, tc := range cases {
		if got := prior.pasteRefusal(tc.paste); !strings.Contains(got, tc.want) {
			t.Errorf("refusal of %.12q = %q, want it to name %q", tc.paste, got, tc.want)
		}
	}
}

func TestComposerPasteRefusalKeepsTheDraftAndCaret(t *testing.T) {
	t.Parallel()
	m := typeText(t, paletteModel(t), "abc")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyLeft})
	m, _ = drive(t, m, tea.PasteMsg{Content: pasteLines(12)})
	before := "ab" + firstPasteChip + "|c"
	if got := caretShown(t, m); got != before {
		t.Fatalf("setup draft with the caret typed as | = %q, want %q", got, before)
	}
	for _, paste := range []string{"x\x00y", "x\xe4\xb8", strings.Repeat("x", pasteLimit+1)} {
		refused, _ := drive(t, m, tea.PasteMsg{Content: paste})
		if got := caretShown(t, refused); got != before {
			t.Errorf("refused paste %.12q left %q, want the draft and caret %q", paste, got, before)
		}
		if refused.notice == "" {
			t.Errorf("refused paste %.12q showed no notice", paste)
		}
	}
}

func TestCanonicalControlsSurviveEditAndSend(t *testing.T) {
	t.Parallel()
	texts := []string{"\u1112\u1161\u11ab", "\U0001F468\u200D\U0001F469\u200D\U0001F467", "1\uFE0F\u20E3", "a\u200Eb"}
	for _, text := range texts {
		t.Run(text, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := newTestModel(caller, Options{})
			m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
			m.thread.id = "t1"
			m = typeText(t, m, text)
			if got := m.input.composer.Value(); got != text {
				t.Fatalf("composer holds %q, want %q", got, text)
			}
			m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyLeft})
			m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyRight})
			if got := m.input.composer.Value(); got != text {
				t.Fatalf("composer holds %q after moving, want %q", got, text)
			}
			m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
			runCommand(t, m, command, "Enter did not send the prompt")
			requireLastPrompt(t, caller.calls, 1, "turn/start", text)
		})
	}
}

func TestRenderedControlsEscapeWithoutRewritingCanonical(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m = typeText(t, m, "a\x01b\x7fc")
	if got := m.input.composer.Value(); got != "a\x01b\x7fc" {
		t.Fatalf("composer holds %q, want the controls kept in canonical text", got)
	}
	shown := stripANSI(strings.Join(m.dockLines(), "\n"))
	if strings.Contains(shown, "\x01") || strings.Contains(shown, "\x7f") {
		t.Fatalf("composer row executes raw controls: %q", shown)
	}
	if !strings.Contains(shown, "<U+0001>") || !strings.Contains(shown, "<U+007F>") {
		t.Fatalf("composer row lacks visible escapes: %q", shown)
	}
}

func TestCaretOffsetsMatchCanonicalScalars(t *testing.T) {
	t.Parallel()
	const text = "e\u0301x\x01y"
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m.input.composer.SetValue(text)
	m.input.composer.cursor = len([]rune(text))
	view := m.View()
	if view.Cursor == nil {
		t.Fatal("cursor unset")
	}
	want := promptCells + len("e") + len("x") + len("<U+0001>") + len("y")
	if view.Cursor.X != want {
		t.Fatalf("cursor at X %d, want %d after %q", view.Cursor.X, want, text)
	}
	if got := m.input.composer.byteCursor(); got != len(text) {
		t.Fatalf("byte cursor at %d, want %d for the canonical text", got, len(text))
	}
}
