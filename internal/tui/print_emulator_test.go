package tui

import (
	"bytes"
	"fmt"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/charmbracelet/x/vt"
)

const (
	emuWidth      = 40
	emuHeight     = 8
	emuWaitBudget = 10 * time.Second
)

type lockedNewline struct {
	mu *sync.Mutex
	em *vt.Emulator
}

func (w lockedNewline) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	text := bytes.ReplaceAll(p, []byte("\r\n"), []byte("\n"))
	text = bytes.ReplaceAll(text, []byte("\n"), []byte("\r\n"))
	if _, err := w.em.Write(text); err != nil {
		return 0, err
	}
	return len(p), nil
}

type probe struct {
	read  func(m model) any
	reply chan any
}

type injection struct {
	msg      tea.Msg
	printing atomic.Bool
}

type noInit struct {
	model
	inject *injection
}

func (n noInit) Init() tea.Cmd { return nil }

func (n noInit) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	if p, ok := msg.(probe); ok {
		p.reply <- p.read(n.model)
		return n, nil
	}
	next, command := n.model.Update(msg)
	n.model = next.(model)
	if _, ok := msg.(printedMsg); ok && n.inject != nil && n.inject.msg != nil && n.out.printing {
		injected := n.inject.msg
		n.inject.msg = nil
		n.inject.printing.Store(n.out.printing)
		next, more := n.model.Update(injected)
		n.model = next.(model)
		command = tea.Batch(command, more)
	}
	return n, command
}

type emulatedRun struct {
	program *tea.Program
	em      *vt.Emulator
	mu      *sync.Mutex
	width   int
	done    chan struct{}
}

func startEmulated(width, height int, inject *injection) *emulatedRun {
	var mu sync.Mutex
	em := vt.NewEmulator(width, height)
	program := tea.NewProgram(
		noInit{model: newModel(new(fakeCaller), Options{}, nil), inject: inject},
		tea.WithInput(nil),
		tea.WithOutput(lockedNewline{mu: &mu, em: em}),
		tea.WithWindowSize(width, height),
		tea.WithoutSignals(),
		tea.WithoutSignalHandler(),
		tea.WithFPS(framesPerSecond),
	)
	run := &emulatedRun{program: program, em: em, mu: &mu, width: width, done: make(chan struct{})}
	go func() {
		defer close(run.done)
		_, _ = program.Run()
	}()
	return run
}

func (r *emulatedRun) send(msg tea.Msg) {
	r.program.Send(msg)
}

func (r *emulatedRun) read(read func(m model) any) any {
	reply := make(chan any, 1)
	r.send(probe{read: read, reply: reply})
	return <-reply
}

func (r *emulatedRun) dock() []string {
	return r.read(func(m model) any { return m.dockLines() }).([]string)
}

func (r *emulatedRun) waitFor(t *testing.T, what string, ok func(history, screen string) bool) {
	t.Helper()
	deadline := time.Now().Add(emuWaitBudget)
	for {
		synctest.Wait()
		history, screen := r.history(), r.screen()
		if ok(history, screen) {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s:\nhistory:\n%s\nscreen:\n%s", what, history, screen)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func (r *emulatedRun) stop() {
	r.program.Quit()
	<-r.done
	time.Sleep(time.Second)
	synctest.Wait()
}

func (r *emulatedRun) history() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	var b strings.Builder
	for y := range r.em.ScrollbackLen() {
		for x := range r.width {
			if cell := r.em.ScrollbackCellAt(x, y); cell != nil {
				b.WriteString(cell.Content)
			}
		}
		b.WriteByte('\n')
	}
	return b.String()
}

func (r *emulatedRun) screen() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.em.String()
}

func commandDone(command string) notificationMsg {
	return commandOutput(command, "ok")
}

func commandOutput(command, output string) notificationMsg {
	id := "c-" + strings.ReplaceAll(command, " ", "-")
	return notificationMsg{method: "item/completed", params: []byte(fmt.Sprintf(`{"threadId":"t1","turnId":"turn-1","completedAtMs":4,"item":{"type":"commandExecution","id":%q,"command":%q,"commandActions":[],"cwd":"/work","status":"completed","aggregatedOutput":%q,"exitCode":0}}`, id, command, output))}
}

func numberedOutput(lines int) string {
	var b strings.Builder
	for i := 1; i <= lines; i++ {
		fmt.Fprintf(&b, "out-%02d\n", i)
	}
	return strings.TrimSuffix(b.String(), "\n")
}

func approvalRequest(command string) serverRequestMsg {
	return serverRequestMsg{
		method: "item/commandExecution/requestApproval",
		answer: make(chan requestAnswer, 1),
		params: []byte(fmt.Sprintf(`{"threadId":"t1","turnId":"turn-1","itemId":"cmd1","startedAtMs":1,"command":%q}`, command)),
	}
}

func rowCount(history, screen string, row string) int {
	return strings.Count(history+screen, row)
}

func leakedLine(history string, dock []string) string {
	for _, line := range dock {
		if text := strings.TrimSpace(ansi.Strip(line)); text != "" && strings.Contains(history, text) {
			return text
		}
	}
	return ""
}

func TestPrintAfterDockShrinkLeavesNoDockInHistory(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		inject := &injection{msg: press('n', 0, "n")}
		run := startEmulated(emuWidth, 24, inject)
		defer run.stop()
		run.send(commandDone("seq 30"))
		run.waitFor(t, "first row printed", func(h, s string) bool { return rowCount(h, s, "seq 30") == 1 })
		run.send(approvalRequest("make all"))
		run.waitFor(t, "approval card open", func(_, s string) bool { return strings.Contains(s, "approval required") })
		openDock := run.dock()
		run.send(commandOutput("seq 20", numberedOutput(20)))
		run.waitFor(t, "rows printed and approval closed", func(h, s string) bool {
			return rowCount(h, s, "out-20") == 1 && !strings.Contains(s, "approval required")
		})
		if !inject.printing.Load() {
			t.Fatal("the approval closed when no print was in flight")
		}
		closedDock := run.dock()
		run.stop()
		history := run.history()
		if line := leakedLine(history, openDock); line != "" {
			t.Fatalf("open dock line %q in history:\n%s", line, history)
		}
		if line := leakedLine(history, closedDock); line != "" {
			t.Fatalf("dock line %q in history:\n%s", line, history)
		}
	})
}

func TestDockGrowDuringPrintKeepsHistoryClean(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		inject := &injection{msg: approvalRequest("make all")}
		run := startEmulated(emuWidth, emuHeight, inject)
		defer run.stop()
		run.send(commandDone("seq 30"))
		run.waitFor(t, "first row printed", func(h, s string) bool { return rowCount(h, s, "seq 30") == 1 })
		run.send(commandOutput("seq 20", numberedOutput(20)))
		run.waitFor(t, "approval card open during a print", func(h, s string) bool {
			return inject.printing.Load() && strings.Contains(s, "approval required")
		})
		openDock := run.dock()
		run.stop()
		history := run.history()
		if line := leakedLine(history, openDock); line != "" {
			t.Fatalf("open dock line %q in history:\n%s", line, history)
		}
	})
}
