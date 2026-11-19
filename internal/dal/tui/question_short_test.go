package tui

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

func shortQuestionModel(t *testing.T, width, height int) model {
	t.Helper()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: width, Height: height})
	m, _ = drive(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)})
	m, _ = drive(t, m, questionRequest(make(chan requestAnswer, 1), questionParamsJSON))
	if len(m.approvals) != 1 || m.approvals[0].card == nil {
		t.Fatalf("question was not queued: %#v", m.approvals)
	}
	return m
}

type shortQuestion struct {
	height int
	panel  int
	option bool
	live   bool
	band   bool
}

func (c shortQuestion) check(t *testing.T, lines []string) {
	t.Helper()
	shortDock{panel: c.panel, live: c.live, band: c.band}.check(t, lines, "┌ Question 1 of")
	c.checkCard(t, strings.Join(lines[1:1+c.panel], "\n"))
}

func (c shortQuestion) checkCard(t *testing.T, card string) {
	t.Helper()
	if !strings.Contains(card, "s submit") {
		t.Fatalf("card lacks %q:\n%s", "s submit", card)
	}
	if got := strings.Contains(card, "1. SQL"); got != c.option {
		t.Fatalf("card shows the option under the cursor = %v, want %v:\n%s", got, c.option, card)
	}
}

func TestQuestionCardKeepsOptionAndActionsAtHeights(t *testing.T) {
	t.Parallel()
	for _, tc := range []shortQuestion{
		{height: 4, panel: 2},
		{height: 5, panel: 3, option: true},
		{height: 6, panel: 3, option: true, live: true},
		{height: 7, panel: 3, option: true, live: true, band: true},
		{height: 8, panel: 4, option: true, live: true, band: true},
	} {
		for _, width := range []int{20, 40} {
			t.Run(fmt.Sprintf("%dx%d", width, tc.height), func(t *testing.T) {
				t.Parallel()
				view := shortQuestionModel(t, width, tc.height).View().Content
				assertFrame(t, strings.Split(view, "\n"), width, tc.height)
				tc.check(t, strings.Split(ansi.Strip(view), "\n"))
			})
		}
	}
}

func TestQuestionCardNeedsFourRows(t *testing.T) {
	t.Parallel()
	for _, height := range []int{2, 3} {
		t.Run(fmt.Sprintf("height %d", height), func(t *testing.T) {
			t.Parallel()
			view := shortQuestionModel(t, 40, height).View().Content
			assertFrame(t, strings.Split(view, "\n"), 40, height)
			if screen := ansi.Strip(view); strings.Contains(screen, "Question 1") || strings.Contains(screen, "s submit") {
				t.Fatalf("a window of %d rows shows the card:\n%s", height, screen)
			}
		})
	}
}

func scrollKeepsShortQuestionFrame(t *testing.T, keys []tea.KeyPressMsg, width, height int) {
	t.Helper()
	m := shortQuestionModel(t, width, height)
	for _, key := range keys {
		m, _ = drive(t, m, key)
	}
	before := len(m.dockLines())
	assertFrame(t, strings.Split(m.View().Content, "\n"), width, height)
	m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
	assertFrame(t, strings.Split(m.View().Content, "\n"), width, height)
	if got := len(m.dockLines()); got != before {
		t.Fatalf("dock changed from %d to %d rows after a scroll key", before, got)
	}
}

func TestQuestionCardFramesAtShortHeights(t *testing.T) {
	t.Parallel()
	tabs := []struct {
		name string
		keys []tea.KeyPressMsg
	}{
		{"single", nil},
		{"multi", []tea.KeyPressMsg{press(tea.KeyTab, 0, "")}},
		{"review", []tea.KeyPressMsg{press(tea.KeyTab, 0, ""), press(tea.KeyTab, 0, "")}},
		{"typing", []tea.KeyPressMsg{press('3', 0, "3")}},
	}
	for _, tab := range tabs {
		for height := 4; height <= 9; height++ {
			for _, width := range []int{20, 40, 100} {
				t.Run(fmt.Sprintf("%s %dx%d", tab.name, width, height), func(t *testing.T) {
					t.Parallel()
					scrollKeepsShortQuestionFrame(t, tab.keys, width, height)
				})
			}
		}
	}
}
