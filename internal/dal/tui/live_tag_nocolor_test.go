package tui

import (
	"strings"
	"testing"
	"testing/synctest"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/colorprofile"
)

const emulatorFaint = 2

func liveRowCells(t *testing.T, run *emulatedRun, text string) (y, x int) {
	t.Helper()
	run.mu.Lock()
	defer run.mu.Unlock()
	for row, line := range strings.Split(run.em.String(), "\n") {
		if before, _, found := strings.Cut(line, text); found {
			return row, len([]rune(before))
		}
	}
	t.Fatalf("screen has no %q:\n%s", text, run.em.String())
	return 0, 0
}

func liveRowAttrs(run *emulatedRun, y, x, width int) []uint8 {
	run.mu.Lock()
	defer run.mu.Unlock()
	attrs := make([]uint8, 0, width)
	for i := range width {
		attrs = append(attrs, run.em.CellAt(x+i, y).Style.Attrs)
	}
	return attrs
}

func requireCellAttrs(t *testing.T, run *emulatedRun, text string, want uint8, what string) {
	t.Helper()
	y, x := liveRowCells(t, run, text)
	for i, attr := range liveRowAttrs(run, y, x, len(text)) {
		if attr&want != want {
			t.Fatalf("%s cell %d has attributes %b, want %b with colour off", what, i, attr, want)
		}
	}
}

func requireColourOffLiveRow(t *testing.T, profile colorprofile.Profile, profileFirst bool) {
	t.Helper()
	run := startEmulated(80, 24, nil)
	defer run.stop()
	run.send(commandDone("seq 1"))
	run.waitFor(t, "first row printed", func(h, s string) bool { return rowCount(h, s, "seq 1") == 1 })
	if profileFirst {
		run.send(tea.ColorProfileMsg{Profile: profile})
	}
	run.send(tea.EnvMsg([]string{"TERM=dumb", "NO_COLOR=1"}))
	if !profileFirst {
		synctest.Wait()
		run.send(tea.ColorProfileMsg{Profile: profile})
	}
	run.send(notificationMsg{method: "turn/started", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`)})
	waitModel(t, run, "turn running", func(m model) bool { return m.thread.state == stateRunning })
	time.Sleep(12 * time.Second)
	run.waitFor(t, "live row with elapsed", func(_, screen string) bool {
		return strings.Contains(screen, "Working") && strings.Contains(screen, "12s")
	})
	requireCellAttrs(t, run, "Working", emulatorBold|emulatorReverse, "tag")
	requireCellAttrs(t, run, "12s", emulatorFaint, "elapsed")
	if raw := run.raw(); colourSGR(t, raw) {
		t.Fatalf("colour-off output holds a colour sequence:\n%q", raw)
	}
}

func TestColourOffKeepsLiveTagReverseOnDumbTerminal(t *testing.T) {
	t.Parallel()
	for _, profile := range []colorprofile.Profile{colorprofile.NoTTY, colorprofile.ASCII} {
		for _, profileFirst := range []bool{true, false} {
			order := "ProfileAfterEnv"
			if profileFirst {
				order = "ProfileBeforeEnv"
			}
			t.Run(profile.String()+"/"+order, func(t *testing.T) {
				t.Parallel()
				synctest.Test(t, func(t *testing.T) { requireColourOffLiveRow(t, profile, profileFirst) })
			})
		}
	}
}
