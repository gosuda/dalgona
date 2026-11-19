package tui

import (
	"fmt"
	"strings"
	"testing"
	"testing/synctest"
	"time"

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
	message := command().(tea.BatchMsg)[0]().(tea.RawMsg)
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

const (
	relocateWidth  = 60
	relocateHeight = 12
	relocateTop    = 4
)

func relocateRows() map[string][]string {
	hint := "\x1b[38;2;159;157;150mSend a prompt · / commands · ? keys\x1b[0m"
	notice := "\x1b[38;2;159;157;150mDraft saved to history · Ctrl+C again to exit\x1b[0m"
	band := "\x1b[38;2;159;157;150mdefault ⊡" + strings.Repeat(" ", 20) + "dal\x1b[0m"
	return map[string][]string{
		"composer shorter than the notice below it": {"\x1b[38;2;58;169;159m› \x1b[0m" + hint, notice, band},
		"band mark under a shorter row":             {"› short", strings.Repeat(" ", relocateWidth-2), band},
		"every row longer than the one above it":    {"› a", "bb bb bb bb", notice, band},
		"single long row":                           {"› " + strings.Repeat("x", relocateWidth-4)},
		"short rows under a long one":               {notice, "ab", "c"},
	}
}

func paintAbsolute(em *vt.Emulator, rows []string, top int) {
	for i, row := range rows {
		_, _ = fmt.Fprintf(em, "\x1b[%d;1H%s", top+i+1, row)
	}
}

func cellText(em *vt.Emulator, x, y int) string {
	cell := em.CellAt(x, y)
	if cell == nil || cell.IsZero() {
		return ""
	}
	return fmt.Sprintf("%q %v", cell.Content, cell.Style)
}

func relocatedAndCleanScreens(rows []string, caretRow, distance int) (relocated, clean *vt.Emulator, stream string) {
	const transcript = "\x1b[1;1Htranscript one\x1b[2;1Htranscript two"
	const column = 2
	relocated = vt.NewEmulator(relocateWidth, relocateHeight)
	clean = vt.NewEmulator(relocateWidth, relocateHeight)
	for _, screen := range []*vt.Emulator{relocated, clean} {
		_, _ = screen.WriteString(transcript)
	}
	paintAbsolute(relocated, rows, relocateTop)
	paintAbsolute(clean, rows, relocateTop+distance)
	_, _ = fmt.Fprintf(relocated, "\x1b[%d;%dH", relocateTop+caretRow+1, column+1)
	_, _ = fmt.Fprintf(clean, "\x1b[%d;%dH", relocateTop+distance+caretRow+1, column+1)
	position := dockPosition{geometry: frameGeometry{rows: len(rows)}, column: column, row: caretRow}
	stream = relocateDock(strings.Join(rows, "\n"), position, distance)
	_, _ = relocated.WriteString(stream)
	return relocated, clean, stream
}

func assertSameCells(t *testing.T, got, want *vt.Emulator) {
	t.Helper()
	for y := range relocateHeight {
		for x := range relocateWidth {
			if gotCell, wantCell := cellText(got, x, y), cellText(want, x, y); gotCell != wantCell {
				t.Fatalf("cell (%d,%d) = %s, want %s:\n%s\nwant:\n%s", x, y, gotCell, wantCell, got.String(), want.String())
			}
		}
	}
	if gotCursor, wantCursor := got.CursorPosition(), want.CursorPosition(); gotCursor != wantCursor {
		t.Fatalf("cursor = %v, want %v", gotCursor, wantCursor)
	}
}

type relocateCase struct {
	name     string
	rows     []string
	caretRow int
	distance int
}

func relocateCases() []relocateCase {
	var cases []relocateCase
	for name, rows := range relocateRows() {
		for _, caretRow := range []int{0, len(rows) - 1} {
			for distance := 1; distance <= relocateHeight-relocateTop-len(rows); distance++ {
				cases = append(cases, relocateCase{name: fmt.Sprintf("%s caret row %d distance %d", name, caretRow, distance), rows: rows, caretRow: caretRow, distance: distance})
			}
		}
	}
	return cases
}

func TestRelocateDockLeavesNoStaleCellFromTheOldRows(t *testing.T) {
	t.Parallel()
	for _, c := range relocateCases() {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			relocated, clean, stream := relocatedAndCleanScreens(c.rows, c.caretRow, c.distance)
			defer relocated.Close()
			defer clean.Close()
			for _, forbidden := range []string{"\x1b[2J", "\x1b[3J", "\x1b[H"} {
				if strings.Contains(stream, forbidden) {
					t.Fatalf("forbidden sequence %q in %q", forbidden, stream)
				}
			}
			assertSameCells(t, relocated, clean)
			if relocated.ScrollbackLen() != 0 {
				t.Fatal("relocation inserted synthetic history")
			}
		})
	}
}

func armDockMove(t *testing.T, m *model) tea.Cmd {
	t.Helper()
	m.anchorDock()
	command := m.positionDock(tea.CursorPositionMsg{X: 2, Y: 0})
	if command == nil {
		t.Fatal("missing dock relocation")
	}
	if !m.anchorState().moving {
		t.Fatal("a dock relocation did not arm a move")
	}
	return command
}

func writeDockMove(t *testing.T, msgs []tea.Msg) {
	t.Helper()
	for _, msg := range msgs {
		if raw, ok := msg.(tea.RawMsg); ok && fmt.Sprint(raw.Msg) == "" {
			t.Fatal("the dock move was dropped as stale before it was written")
		}
	}
}

func requestsCursor(command tea.Cmd) bool {
	for _, msg := range commandMsgs(command) {
		if raw, ok := msg.(tea.RawMsg); ok && fmt.Sprint(raw.Msg) == dockQueryMarker {
			return true
		}
	}
	return false
}

func TestDockMoveTimeoutClearsMoving(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m := anchorTestModel()
		command := armDockMove(t, &m)
		m.commit("held", []string{"row waiting for the move"})
		if m.drain() != nil {
			t.Fatal("a row printed while the move was in flight")
		}
		for _, msg := range commandMsgs(command) {
			m, _ = updateWithCommand(t, m, msg)
		}
		if m.anchorState().moving {
			t.Fatal("a move the terminal never acknowledged still holds the dock after one second")
		}
		if !m.out.printing {
			t.Fatal("the waiting row did not start printing after the move was released")
		}
	})
}

func TestDockMoveTimeoutRequestsTheCursorAgain(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m := anchorTestModel()
		msgs := commandMsgs(armDockMove(t, &m))
		writeDockMove(t, msgs)
		if m.anchorState().needed {
			t.Fatal("a written dock move left the dock marked as misplaced")
		}
		asked := false
		for _, msg := range msgs {
			var next tea.Cmd
			m, next = updateWithCommand(t, m, msg)
			asked = asked || requestsCursor(next)
		}
		if !asked {
			t.Fatal("an unacknowledged move did not request the cursor position again")
		}
	})
}

func TestDockMoveTimeoutIgnoresAnAcknowledgedMove(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m := anchorTestModel()
		msgs := commandMsgs(armDockMove(t, &m))
		writeDockMove(t, msgs)
		m, _ = updateWithCommand(t, m, dockWrittenMsg{revision: m.anchorState().moveRevision})
		for _, msg := range msgs {
			var next tea.Cmd
			m, next = updateWithCommand(t, m, msg)
			if requestsCursor(next) {
				t.Fatal("the timeout of an acknowledged move requested the cursor position")
			}
		}
		if m.anchorState().needed {
			t.Fatal("the timeout of an acknowledged move marked the dock as misplaced")
		}
	})
}

func TestDockMoveTimeoutIgnoresASupersededMove(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m := anchorTestModel()
		first := commandMsgs(armDockMove(t, &m))
		m, _ = updateWithCommand(t, m, dockWrittenMsg{revision: m.anchorState().moveRevision})
		armDockMove(t, &m)
		revision := m.anchorState().moveRevision
		for _, msg := range first {
			m, _ = updateWithCommand(t, m, msg)
		}
		if !m.anchorState().moving || m.anchorState().moveRevision != revision {
			t.Fatal("the timeout of an earlier move released the move in flight")
		}
	})
}

func commandDelay(command tea.Cmd) time.Duration {
	start := time.Now()
	commandMsgs(command)
	return time.Since(start)
}

func TestDockWaitsLastOneSecond(t *testing.T) {
	t.Parallel()
	for _, wait := range []struct {
		name  string
		start func(t *testing.T) tea.Cmd
	}{
		{name: "query expiry", start: func(*testing.T) tea.Cmd {
			m := anchorTestModel()
			return m.anchorDock()
		}},
		{name: "move release", start: func(t *testing.T) tea.Cmd {
			m := anchorTestModel()
			return armDockMove(t, &m)
		}},
		{name: "rejected reply hold", start: func(t *testing.T) tea.Cmd {
			m := armedQuery(t)
			return m.answerDock(rejectedReply(&m))
		}},
	} {
		t.Run(wait.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				if got := commandDelay(wait.start(t)); got != time.Second {
					t.Fatalf("%s lasts %v, want one second", wait.name, got)
				}
			})
		})
	}
}
