package tui

import (
	"testing"

	tea "charm.land/bubbletea/v2"
)

func paletteModel(t *testing.T) model {
	t.Helper()
	m := newTestModel(nil, Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	return m
}

func typeKeys(t *testing.T, m model, text string) model {
	t.Helper()
	for _, r := range text {
		m, _ = drive(t, m, press(r, 0, string(r)))
	}
	return m
}

func TestSlashPaletteFiltersAndCompletes(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "/mo")
	assertContains(t, dockText(m), "/model")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyTab})
	if got := m.input.composer.Value(); got != "/model " {
		t.Fatalf("tab gave %q, want %q", got, "/model ")
	}
}

func TestPopupStaysOpenOnFirstToken(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "mo")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyHome})
	m = typeKeys(t, m, "/")
	assertContains(t, dockText(m), "/model")
	if _, row, col := m.dock(); row != 0 || col != promptCells+1 {
		t.Fatalf("caret at row %d column %d, want row 0 column %d", row, col, promptCells+1)
	}
}

func TestPopupSteady(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "/")
	if got := len(m.dockLines()); got != 11 {
		t.Fatalf("dock with the palette open has %d lines, want 11", got)
	}
	keys := make([]tea.KeyPressMsg, 0, 100)
	for range 50 {
		keys = append(keys, press('a', 0, "a"))
	}
	for range 50 {
		keys = append(keys, tea.KeyPressMsg{Code: tea.KeyDown})
	}
	assertSteadyDock(t, m, keys)
}

func TestPaletteHighlightMovesWithArrows(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "/")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	assertContains(t, dockText(m), "┃ /resume")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyUp})
	assertContains(t, dockText(m), "┃ /model")
}

func TestPaletteEscKeepsDraftAndTypingReopens(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "/m")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
	if got := len(m.dockLines()); got != 3 {
		t.Fatalf("dock after esc has %d lines, want 3", got)
	}
	if got := m.input.composer.Value(); got != "/m" {
		t.Fatalf("esc changed the draft to %q", got)
	}
	m = typeKeys(t, m, "o")
	assertContains(t, dockText(m), "/model")
}

func TestPaletteClosesAfterFirstToken(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "/mo ")
	if got := len(m.dockLines()); got != 3 {
		t.Fatalf("dock after a space has %d lines, want 3", got)
	}
}

func TestPaletteIgnoresDoubleSlash(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "//")
	if got := len(m.dockLines()); got != 3 {
		t.Fatalf("dock for // has %d lines, want 3", got)
	}
}

func TestPaletteEnterCompletesCommandWithArgument(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "/mo")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	if got := m.input.composer.Value(); got != "/model " {
		t.Fatalf("enter gave %q, want %q", got, "/model ")
	}
	if got := len(m.dockLines()); got != 3 {
		t.Fatalf("dock after completing has %d lines, want 3", got)
	}
}

func TestPaletteCompletionKeepsTextAfterToken(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), " arg")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyHome})
	m = typeKeys(t, m, "/mo")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyTab})
	if got := m.input.composer.Value(); got != "/model arg" {
		t.Fatalf("tab gave %q, want %q", got, "/model arg")
	}
}

func TestPaletteEnterKeepsTextAfterTokenAndRunsNothing(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), " arg")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyHome})
	m = typeKeys(t, m, "/new")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	if got := m.input.composer.Value(); got != "/new arg" {
		t.Fatalf("enter gave %q, want %q", got, "/new arg")
	}
	if got := len(m.input.history.entries); got != 0 {
		t.Fatalf("enter sent a prompt: history holds %d entries", got)
	}
}

func TestPaletteClosedByReview(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, reviewModel(t, 80, 3), "/")
	m = rawUpdate(t, m, press('o', tea.ModCtrl, ""))
	if m.review == nil {
		t.Fatal("ctrl+o did not open the review")
	}
	m = rawUpdate(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
	if m.review != nil {
		t.Fatal("esc did not close the review")
	}
	if got := len(m.dockLines()); got != 3 {
		t.Fatalf("dock after the review closes has %d lines, want 3", got)
	}
}

func TestPaletteWithoutMatchLeavesArrowsToComposer(t *testing.T) {
	t.Parallel()
	m := paletteModel(t)
	m.input.history.add("earlier prompt", "")
	m = typeKeys(t, m, "/zzz")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyUp})
	if got := m.input.composer.Value(); got != "earlier prompt" {
		t.Fatalf("up with no matching command gave %q, want the recalled prompt", got)
	}
}
