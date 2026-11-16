package tui

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/vt"
)

func anchorTestModel() model {
	m := newTestModel(nil, Options{})
	m.screen.width, m.screen.height = 40, 8
	m.out.geometry = m.currentGeometry()
	m.anchorState().needed = true
	return m
}

func TestDockPositionRejectsStaleCursorReply(t *testing.T) {
	t.Parallel()
	for _, change := range []struct {
		name  string
		apply func(*model)
	}{
		{name: "resized away and back", apply: func(m *model) { m.out.settleN += 2 }},
		{name: "history printed", apply: func(m *model) { m.anchorState().prints++ }},
		{name: "caret moved", apply: func(m *model) { m.input.composer.SetValue("x") }},
	} {
		t.Run(change.name, func(t *testing.T) {
			m := anchorTestModel()
			if m.anchorDock() == nil {
				t.Fatal("missing cursor query")
			}
			change.apply(&m)
			if m.positionDock(tea.CursorPositionMsg{X: 2, Y: 0}) != nil {
				t.Fatal("a stale cursor reply relocated the dock")
			}
		})
	}
}

func TestDockMoveRejectsInterveningInput(t *testing.T) {
	t.Parallel()
	m := anchorTestModel()
	m.anchorDock()
	command := m.positionDock(tea.CursorPositionMsg{X: 2, Y: 0})
	if command == nil {
		t.Fatal("missing dock relocation")
	}
	message := command().(tea.RawMsg)
	m = rawUpdate(t, m, press('x', 0, "x"))
	if written := fmt.Sprint(message.Msg); written != "" {
		t.Fatalf("stale dock bytes would overwrite new input: %q", written)
	}
	if m.input.composer.Value() != "x" {
		t.Fatal("input was lost during relocation")
	}
}

func TestUnansweredCursorQueryDoesNotBlockPrinting(t *testing.T) {
	t.Parallel()
	m := anchorTestModel()
	m.anchorDock()
	m.commit("unanswered", []string{"still prints"})
	if m.drain() == nil {
		t.Fatal("an unanswered cursor query blocked permanent output")
	}
}

func TestRelocateDockClearsRestoredRowsWithoutScrolling(t *testing.T) {
	t.Parallel()
	em := vt.NewEmulator(40, 12)
	defer em.Close()
	_, _ = em.WriteString("\x1b[1;1Htranscript\x1b[5;1Hold dock\x1b[9;1Hrestored dock\x1b[5;3H")
	position := dockPosition{geometry: frameGeometry{rows: 3}, column: 2}
	_, _ = em.WriteString(relocateDock("> \n\nstatus", position, 5))
	screen := em.String()
	if !strings.Contains(screen, "transcript") || strings.Contains(screen, "dock") {
		t.Fatalf("relocation changed transcript or retained old dock rows:\n%s", screen)
	}
	if cursor := em.CursorPosition(); cursor.X != 2 || cursor.Y != 9 {
		t.Fatalf("cursor after relocation = %v, want (2,9)", cursor)
	}
	if em.ScrollbackLen() != 0 {
		t.Fatal("relocation inserted synthetic history")
	}
}
