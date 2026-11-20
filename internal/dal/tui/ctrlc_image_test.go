package tui

import (
	"path/filepath"
	"slices"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func imageDraftModel(t *testing.T, typed string) model {
	t.Helper()
	m := imageModel(t, new(fakeCaller), "text", "image")
	m = typeText(t, m, typed)
	path := writePNG(t, filepath.Join(t.TempDir(), "shot.png"), 3, 2, 0)
	m, command := drive(t, m, tea.PasteMsg{Content: path})
	m = runCommand(t, m, command, "pasting an image path started no read")
	if got := len(m.input.composer.images()); got != 1 {
		t.Fatalf("draft holds %d images, want 1", got)
	}
	return m
}

func TestCtrlCKeepsADraftThatHoldsNoText(t *testing.T) {
	t.Parallel()
	blankPaste := strings.Repeat("\n", 11)
	tests := []struct {
		name  string
		model func(*testing.T) model
	}{
		{"image only", func(t *testing.T) model { return imageDraftModel(t, "") }},
		{"blank paste chip", func(t *testing.T) model {
			m, _ := drive(t, paletteModel(t), tea.PasteMsg{Content: blankPaste})
			if len(m.input.composer.chips) != 1 {
				t.Fatalf("composer holds %d chips, want 1", len(m.input.composer.chips))
			}
			return m
		}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			m := tt.model(t)
			before := m.input.composer.Value()
			m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
			if m.hint.text != "Ctrl+C again to exit" {
				t.Fatalf("hint = %q, want Ctrl+C again to exit", m.hint.text)
			}
			if got := historyTexts(m); len(got) != 0 {
				t.Fatalf("history = %q, want nothing saved", got)
			}
			if got := m.input.composer.Value(); got != before || len(m.input.composer.chips) != 1 {
				t.Fatalf("draft %q with %d chips after Ctrl+C, want the draft kept", got, len(m.input.composer.chips))
			}
		})
	}
}

func TestCtrlCSavesTextAndSaysImagesAreDropped(t *testing.T) {
	t.Parallel()
	want := "Draft saved to history, images dropped · Ctrl+C again to exit"
	m := imageDraftModel(t, "note ")
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	if m.hint.text != want {
		t.Fatalf("hint = %q, want %q", m.hint.text, want)
	}
	if got := historyTexts(m); !slices.Equal(got, []string{"note"}) {
		t.Fatalf("history = %q, want the typed text", got)
	}
	if got := m.input.composer.Value(); got != "" || len(m.input.composer.images()) != 0 {
		t.Fatalf("draft %q with %d images after Ctrl+C, want it cleared", got, len(m.input.composer.images()))
	}
	if row := stripANSI(m.liveRow()); !strings.Contains(row, want) {
		t.Fatalf("live row at width 80 = %q, want the whole hint %q", row, want)
	}
}
