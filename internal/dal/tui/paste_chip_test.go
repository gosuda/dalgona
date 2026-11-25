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
