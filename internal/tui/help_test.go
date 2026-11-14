package tui

import (
	"slices"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func helpModel(t *testing.T) model {
	t.Helper()
	m := bandModel("/work/ws")
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, press('?', 0, "?"))
	return m
}

func TestHelpOverlayListsKeysAndCloses(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, press('?', 0, "?"))
	assertContains(t, m.View().Content, "Compose", "sends the prompt")
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if strings.Contains(m.View().Content, "Compose") {
		t.Fatalf("help panel stayed open after esc:\n%s", m.View().Content)
	}
}

func TestHelpStatusLegendPerShownItem(t *testing.T) {
	t.Parallel()
	m := bandModel("/work/ws")
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, resultMsg{op: "settings", value: &settingsReply{Settings: []settingValue{{Key: "statusHide", Value: []any{"folder"}}}}})
	m, _ = drive(t, m, press('?', 0, "?"))
	for range 50 {
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	}
	view := m.View().Content
	assertContains(t, view, "Status", "model gpt-test", "effort high", "approval auto", "sandbox workspace-write", "queue 2", "context 25%", "mark dalgona")
	if strings.Contains(view, "folder") {
		t.Fatalf("hidden folder item has a legend line:\n%s", view)
	}
	body := m.helpBody()
	start := slices.Index(body, "Status")
	if start < 0 {
		t.Fatalf("help body has no Status header:\n%s", strings.Join(body, "\n"))
	}
	if got := len(body) - start - 1; got != 7 {
		t.Fatalf("status legend has %d lines, want 7:\n%s", got, strings.Join(body[start+1:], "\n"))
	}
}

func TestHelpScrollReachesLastLine(t *testing.T) {
	t.Parallel()
	m := helpModel(t)
	if strings.Contains(m.View().Content, "mark dalgona") {
		t.Fatalf("last legend line is visible before scrolling:\n%s", m.View().Content)
	}
	for range 50 {
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	}
	assertContains(t, m.View().Content, "mark dalgona")
}

func TestHelpDockSteady(t *testing.T) {
	t.Parallel()
	m := helpModel(t)
	if got := len(m.dockLines()); got != 17 {
		t.Fatalf("help dock has %d rows, want 17", got)
	}
	keys := make([]tea.KeyPressMsg, 50)
	for i := range keys {
		keys[i] = tea.KeyPressMsg{Code: tea.KeyDown}
	}
	assertSteadyDock(t, m, keys)
}

func TestHelpDockKeepsInputLineWithPaste(t *testing.T) {
	t.Parallel()
	m := helpModel(t)
	m, _ = drive(t, m, tea.PasteMsg{Content: "one\ntwo\nthree"})
	if got := len(m.dockLines()); got != 17 {
		t.Fatalf("help dock with a pasted draft has %d rows, want 17", got)
	}
}

func TestHelpKeyColumnHasSpace(t *testing.T) {
	t.Parallel()
	m := helpModel(t)
	for _, line := range m.helpBody() {
		if strings.HasPrefix(line, "  up, ctrl+p, down, ctrl+n") {
			if !strings.HasPrefix(line, "  up, ctrl+p, down, ctrl+n move the list cursor") {
				t.Fatalf("key and action run together: %q", line)
			}
			return
		}
	}
	t.Fatal("help body has no row for the list cursor keys")
}

func TestHelpNotOpenedInShellMode(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, press('!', 0, "!"))
	m, _ = drive(t, m, press('?', 0, "?"))
	if strings.Contains(m.View().Content, "Compose") {
		t.Fatalf("? opened help in shell mode:\n%s", m.View().Content)
	}
	if got := m.input.composer.Value(); got != "?" {
		t.Fatalf("shell line = %q, want the typed ?", got)
	}
}
