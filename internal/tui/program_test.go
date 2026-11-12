package tui

import (
	"bytes"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	tea "charm.land/bubbletea/v2"
)

type recordedOutput struct {
	mu   sync.Mutex
	data bytes.Buffer
}

func (r *recordedOutput) Write(p []byte) (int, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.data.Write(p)
}

func (r *recordedOutput) String() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.data.String()
}

func startRecorded(width, height int, out *recordedOutput) *emulatedRun {
	program := tea.NewProgram(
		noInit{model: newModel(new(fakeCaller), Options{}, nil)},
		tea.WithInput(nil),
		tea.WithOutput(out),
		tea.WithWindowSize(width, height),
		tea.WithoutSignals(),
		tea.WithoutSignalHandler(),
		tea.WithFPS(framesPerSecond),
	)
	run := &emulatedRun{program: program, done: make(chan struct{})}
	go func() {
		defer close(run.done)
		_, _ = program.Run()
	}()
	return run
}

func waitModel(t *testing.T, run *emulatedRun, what string, ok func(m model) bool) {
	t.Helper()
	deadline := time.Now().Add(emuWaitBudget)
	for {
		synctest.Wait()
		if run.read(func(m model) any { return ok(m) }).(bool) {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", what)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func outsideAlternateScreen(written string) string {
	const enter, leave = "\x1b[?1049h", "\x1b[?1049l"
	before, rest, found := strings.Cut(written, enter)
	if !found {
		return written
	}
	_, after, _ := strings.Cut(rest, leave)
	return before + after
}

func assertClientBytes(t *testing.T, written string) {
	t.Helper()
	outside := outsideAlternateScreen(written)
	for _, seq := range []string{"\x1b[2J", "\x1b[3J", "\x1bc", "\x1b[H", "\x1b[1;1H"} {
		if strings.Contains(outside, seq) {
			t.Fatalf("the client wrote %q while the alternate screen was off", seq)
		}
	}
	if n := strings.Count(written, "\x1b[?1049h"); n != 1 {
		t.Fatalf("the client entered the alternate screen %d times, want 1 for the review", n)
	}
	if n := strings.Count(written, "\x1b[?1049l"); n != 1 {
		t.Fatalf("the client left the alternate screen %d times, want 1 after the review", n)
	}
}

func TestInvI4NoClearOrResetBytes(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		out := new(recordedOutput)
		run := startRecorded(100, 24, out)
		defer run.stop()
		run.send(commandDone("seq 3"))
		waitModel(t, run, "first row printed", func(m model) bool { return m.out.started })
		run.send(tea.WindowSizeMsg{Width: 40, Height: 24})
		waitModel(t, run, "narrowed", func(m model) bool { return m.screen.width == 40 && !m.out.settling })
		run.send(tea.WindowSizeMsg{Width: 100, Height: 24})
		waitModel(t, run, "widened", func(m model) bool { return m.screen.width == 100 && !m.out.settling })
		run.send(press('o', tea.ModCtrl, ""))
		waitModel(t, run, "review open", func(m model) bool { return m.review != nil })
		time.Sleep(time.Second)
		synctest.Wait()
		run.send(press(tea.KeyEscape, 0, ""))
		waitModel(t, run, "review closed", func(m model) bool {
			return m.review == nil && !m.out.settling && !m.out.printing && len(m.out.pending) == 0
		})
		time.Sleep(time.Second)
		synctest.Wait()
		run.stop()
		assertClientBytes(t, out.String())
	})
}
