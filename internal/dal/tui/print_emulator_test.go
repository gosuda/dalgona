package tui

import (
	"bytes"
	"fmt"
	"io"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/charmbracelet/x/vt"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	emuWidth      = 40
	emuHeight     = 8
	emuWaitBudget = 10 * time.Second
)

type lockedNewline struct {
	mu  *sync.Mutex
	em  *vt.Emulator
	raw []byte
}

func (w *lockedNewline) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.raw = append(w.raw, p...)
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

type noopMsg struct{}

func (n noInit) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	if p, ok := msg.(probe); ok {
		p.reply <- p.read(n.model)
		return n, nil
	}
	if mutate, ok := msg.(func(*model)); ok {
		mutate(&n.model)
		msg = noopMsg{}
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
	wire    *lockedNewline
	width   int
	done    chan struct{}
}

func tmuxMode(em *vt.Emulator) {
	em.RegisterCsiHandler(ansi.Command('?', 0, 'n'), func(ansi.Params) bool { return true })
}

func muteMode(em *vt.Emulator) {
	tmuxMode(em)
	em.RegisterCsiHandler('n', func(ansi.Params) bool { return true })
}

func startEmulated(width, height int, inject *injection) *emulatedRun {
	return startEmulatedProgram(vt.NewEmulator(width, height), nil, inject)
}

func startEmulatedProgram(em *vt.Emulator, input io.Reader, inject *injection) *emulatedRun {
	return startEmulatedThrough(em, input, inject, func(wire io.Writer) io.Writer { return wire })
}

func startEmulatedThrough(em *vt.Emulator, input io.Reader, inject *injection, through func(io.Writer) io.Writer) *emulatedRun {
	var mu sync.Mutex
	width, height := em.Width(), em.Height()
	var program *tea.Program
	wire := &lockedNewline{mu: &mu, em: em}
	output := newDockOutput(through(wire), input, func(msg dockWrittenMsg) { program.Send(msg) })
	program = tea.NewProgram(
		noInit{model: newModel(new(fakeCaller), Options{}, nil), inject: inject},
		tea.WithInput(input),
		tea.WithOutput(output),
		tea.WithWindowSize(width, height),
		tea.WithoutSignals(),
		tea.WithoutSignalHandler(),
		tea.WithFPS(framesPerSecond),
	)
	run := &emulatedRun{program: program, em: em, mu: &mu, wire: wire, width: width, done: make(chan struct{})}
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
	if r.em != nil {
		input, ok := r.em.InputPipe().(io.Closer)
		if !ok {
			panic("terminal input pipe cannot close")
		}
		_ = input.Close()
	}
	<-r.done
	if r.em != nil {
		_ = r.em.Close()
	}
	time.Sleep(dockQueryTimeout)
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

func (r *emulatedRun) physicalRow(y int) string {
	r.mu.Lock()
	defer r.mu.Unlock()
	var row strings.Builder
	for x := range r.em.Width() {
		if cell := r.em.CellAt(x, y); cell != nil {
			row.WriteString(cell.Content)
		}
	}
	return row.String()
}

func (r *emulatedRun) screen() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.em.String()
}

func (r *emulatedRun) raw() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return string(r.wire.raw)
}

func commandDone(command string) notificationMsg {
	return commandOutput(command, "ok")
}

func commandOutput(command, output string) notificationMsg {
	return commandOutputIn("turn-1", command, output)
}

func commandOutputIn(turn, command, output string) notificationMsg {
	id := "c-" + strings.ReplaceAll(command, " ", "-")
	return notificationMsg{method: "item/completed", params: []byte(fmt.Sprintf(`{"threadId":"t1","turnId":%q,"completedAtMs":4,"item":{"type":"commandExecution","id":%q,"command":%q,"commandActions":[],"cwd":"/work","status":"completed","aggregatedOutput":%q,"exitCode":0}}`, turn, id, command, output))}
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
		run.waitFor(t, "approval card open", func(_, s string) bool { return strings.Contains(s, "Approval · Run command") })
		openDock := run.dock()
		run.send(commandOutput("seq 20", numberedOutput(20)))
		run.waitFor(t, "rows printed and approval closed", func(h, s string) bool {
			return rowCount(h, s, "out-20") == 1 && !strings.Contains(s, "Approval · Run command")
		})
		if !inject.printing.Load() {
			t.Fatal("the approval closed when no print was in flight")
		}
		closedDock := run.dock()
		run.waitFor(t, "tail rows printed", func(h, s string) bool {
			return rowCount(h, s, "out-20") == 1 && rowCount(h, s, earlierLinesSummary(10)) == 1
		})
		numberedTailOnce(t, run.history(), run.screen(), 20)
		if len(openDock) != 12 {
			t.Fatalf("open dock holds %d lines, want 12", len(openDock))
		}
		if len(closedDock) != 3 {
			t.Fatalf("closed dock holds %d lines, want 3", len(closedDock))
		}
		dockLinesOnScreen(t, closedDock, run.screen())
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
			return inject.printing.Load() && strings.Contains(s, "Approval · Run command")
		})
		openDock := run.dock()
		run.waitFor(t, "tail rows printed", func(h, s string) bool {
			return rowCount(h, s, "out-20") == 1 && rowCount(h, s, earlierLinesSummary(10)) == 1
		})
		numberedTailOnce(t, run.history(), run.screen(), 20)
		want := 1 + panelRows(panelApproval, emuHeight) + 2
		if len(openDock) != want {
			t.Fatalf("open dock holds %d lines, want %d", len(openDock), want)
		}
		dockLinesOnScreen(t, openDock, run.screen())
		run.stop()
		history := run.history()
		if line := leakedLine(history, openDock); line != "" {
			t.Fatalf("open dock line %q in history:\n%s", line, history)
		}
	})
}

type heldRows struct {
	pending  int
	printing bool
}

func TestPrintOnAltScreenExitKeepsRows(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		run := startEmulated(emuWidth, emuHeight, nil)
		defer run.stop()
		run.send(commandDone("seq 1"))
		run.waitFor(t, "first row printed", func(h, s string) bool { return rowCount(h, s, "seq 1") == 1 })
		run.send(press('o', tea.ModCtrl, ""))
		run.waitFor(t, "review open", func(_, _ string) bool {
			return run.read(func(m model) any { return m.review != nil }).(bool)
		})
		waitSettled(run)
		run.send(commitFiveRows)
		held := run.read(func(m model) any {
			return heldRows{pending: len(m.out.pending), printing: m.out.printing}
		}).(heldRows)
		if held.printing || held.pending != 5 {
			t.Fatalf("rows committed while the review is open printed or left the queue: pending %d, printing %t", held.pending, held.printing)
		}
		run.send(press(tea.KeyEscape, 0, ""))
		run.waitFor(t, "five rows printed after the review closes", fiveRowsPrinted)
		if run.altScreen() {
			t.Fatal("the rows were printed while the emulator is on the alternate screen")
		}
	})
}

func earlierLinesSummary(n int) string {
	return fmt.Sprintf("… %d earlier lines", n)
}

func numberedTailOnce(t *testing.T, history, screen string, rows int) {
	t.Helper()
	for i := 1; i <= rows; i++ {
		want := 1
		if i <= rows-outputTailLines {
			want = 0
		}
		if got := rowCount(history, screen, fmt.Sprintf("out-%02d", i)); got != want {
			t.Fatalf("row out-%02d appears %d times, want %d:\nhistory:\n%s\nscreen:\n%s", i, got, want, history, screen)
		}
	}
	if got := rowCount(history, screen, earlierLinesSummary(rows-outputTailLines)); got != 1 {
		t.Fatalf("earlier-lines summary appears %d times, want 1:\nhistory:\n%s\nscreen:\n%s", got, history, screen)
	}
}

func dockLinesOnScreen(t *testing.T, dock []string, screen string) {
	t.Helper()
	plain := ansi.Strip(screen)
	for i, line := range dock {
		if len(dock) >= 2 && i == len(dock)-2 {
			continue
		}
		text := strings.TrimSpace(ansi.Strip(line))
		if text != "" && !strings.Contains(plain, text) {
			t.Fatalf("dock line %q is not on the screen:\n%s", text, screen)
		}
	}
}

func waitSettled(run *emulatedRun) {
	for run.read(func(m model) any { return m.out.settling }).(bool) {
		time.Sleep(10 * time.Millisecond)
	}
}

func commitFiveRows(m *model) {
	for i := 1; i <= 5; i++ {
		m.commit(fmt.Sprintf("five:%d", i), []string{fmt.Sprintf("five-%d", i)})
	}
}

func fiveRowsPrinted(history, screen string) bool {
	for i := 1; i <= 5; i++ {
		if rowCount(history, screen, fmt.Sprintf("five-%d", i)) != 1 {
			return false
		}
	}
	return true
}

func (r *emulatedRun) altScreen() bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.em.IsAltScreen()
}

func dockAtBottom(history, screen string, em *vt.Emulator, height int, run *emulatedRun) bool {
	dock := run.dock()
	if len(dock) == 0 {
		return false
	}
	band := strings.TrimSpace(ansi.Strip(dock[len(dock)-1]))
	run.mu.Lock()
	cursor := em.CursorPosition()
	run.mu.Unlock()
	return band != "" && strings.Contains(run.physicalRow(height-1), "dalgona") &&
		strings.Contains(run.physicalRow(height-1), band) &&
		strings.Count(screen, band) == 1 &&
		leakedLine(history, dock) == "" && cursor.Y == height-3 && cursor.X == 2
}

func commandPrintedOnce(history, screen string) bool {
	return rowCount(history, screen, "anchor-proof") == 1
}

func TestDockAnchorsToPhysicalBottomAcrossResize(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		em := vt.NewEmulator(100, 24)
		tmuxMode(em)
		run := startEmulatedProgram(em, em, nil)
		defer run.stop()
		run.send(resultMsg{op: "initialize", value: &rpc.InitializeResult{UserAgent: "dalgona/test", PlatformFamily: "unix"}})
		run.send(commandDone("anchor-proof"))
		for _, size := range []tea.WindowSizeMsg{
			{Width: 100, Height: 24},
			{Width: 40, Height: 8},
			{Width: 100, Height: 24},
		} {
			run.mu.Lock()
			em.Resize(size.Width, size.Height)
			run.width = size.Width
			run.mu.Unlock()
			run.send(size)
			run.waitFor(t, "status band at physical bottom", func(history, screen string) bool {
				return dockAtBottom(history, screen, em, size.Height, run) &&
					commandPrintedOnce(history, screen)
			})
			run.mu.Lock()
			cursor := em.CursorPosition()
			run.mu.Unlock()
			if cursor.X != 2 || cursor.Y != size.Height-3 {
				t.Fatalf("cursor = %v at %dx%d, want (2,%d)", cursor, size.Width, size.Height, size.Height-3)
			}
		}
	})
}

func bandAtBottom(t *testing.T, run *emulatedRun, em *vt.Emulator, height int, phase string) {
	t.Helper()
	run.waitFor(t, "status band at physical bottom during "+phase, func(history, screen string) bool {
		return dockAtBottom(history, screen, em, height, run)
	})
	run.mu.Lock()
	cursor := em.CursorPosition()
	run.mu.Unlock()
	if cursor.X != 2 || cursor.Y != height-3 {
		t.Fatalf("cursor = %v during %s, want (2,%d)", cursor, phase, height-3)
	}
}

type streamedPart struct {
	key  string
	rows []string
}

func streamPartOnce(t *testing.T, run *emulatedRun, em *vt.Emulator, part streamedPart) {
	t.Helper()
	run.send(func(m *model) { m.commit(part.key, part.rows) })
	run.waitFor(t, part.key+" rows printed", func(history, screen string) bool {
		for _, row := range part.rows {
			if strings.TrimSpace(row) != "" && rowCount(history, screen, row) != 1 {
				return false
			}
		}
		return true
	})
	bandAtBottom(t, run, em, 24, part.key)
}

func streamedRowsPrintedOnce(t *testing.T, run *emulatedRun, turn []streamedPart) {
	t.Helper()
	history, screen := run.history(), run.screen()
	for _, part := range turn {
		for _, row := range part.rows {
			if strings.TrimSpace(row) != "" && rowCount(history, screen, row) != 1 {
				t.Fatalf("row %q appears %d times, want exactly once:\nhistory:\n%s\nscreen:\n%s", row, rowCount(history, screen, row), history, screen)
			}
		}
	}
}

func rawOutputInvariant(t *testing.T, run *emulatedRun) {
	t.Helper()
	for _, forbidden := range []string{"\x1b[2J", "\x1b[3J", "\x1b[H"} {
		if strings.Contains(run.raw(), forbidden) {
			t.Fatalf("forbidden sequence %q in output:\n%q", forbidden, run.raw())
		}
	}
	if got := strings.Count(run.raw(), ansi.RequestCursorPositionReport); got != 1 {
		t.Fatalf("cursor queries at unchanged geometry = %d, want 1", got)
	}
	if got := strings.Count(run.raw(), ansi.RequestExtendedCursorPositionReport); got != 0 {
		t.Fatalf("DECXCPR queries = %d, want 0", got)
	}
}

func TestDockHoldsPhysicalBottomAcrossStreamedTurn(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		em := vt.NewEmulator(100, 24)
		tmuxMode(em)
		run := startEmulatedProgram(em, em, nil)
		defer run.stop()
		run.send(resultMsg{op: "initialize", value: &rpc.InitializeResult{UserAgent: "dalgona/test", PlatformFamily: "unix"}})
		run.send(commandDone("anchor-turn"))
		bandAtBottom(t, run, em, 24, "startup before any input")
		run.send(func(m *model) { m.setState(stateRunning) })
		turn := []streamedPart{
			{key: "echo", rows: []string{"> pin the band", "", "queued"}},
			{key: "chunk-1", rows: []string{"chunk one alpha", "chunk one beta"}},
			{key: "chunk-2", rows: []string{"chunk two alpha", "chunk two beta"}},
			{key: "chunk-3", rows: []string{"chunk three alpha", "chunk three beta"}},
			{key: "final", rows: []string{"final row alpha", "final row beta", "final row gamma"}},
		}
		for _, part := range turn {
			streamPartOnce(t, run, em, part)
		}
		run.send(func(m *model) { m.setState(stateIdle) })
		streamedRowsPrintedOnce(t, run, turn)
		rawOutputInvariant(t, run)
		for index, size := range []tea.WindowSizeMsg{{Width: 40, Height: 8}, {Width: 100, Height: 24}} {
			run.mu.Lock()
			em.Resize(size.Width, size.Height)
			run.width = size.Width
			run.mu.Unlock()
			run.send(size)
			run.waitFor(t, "cursor-position query after resize", func(_, _ string) bool {
				return strings.Count(run.raw(), ansi.RequestCursorPositionReport) == index+2
			})
			bandAtBottom(t, run, em, size.Height, fmt.Sprintf("resize to %dx%d", size.Width, size.Height))
		}
		if got := strings.Count(run.raw(), ansi.RequestCursorPositionReport); got != 3 {
			t.Fatalf("cursor queries across resizes = %d, want 3", got)
		}
	})
}

func TestDockQueryTimeoutAbandonsArming(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		em := vt.NewEmulator(100, 24)
		muteMode(em)
		run := startEmulatedProgram(em, em, nil)
		defer run.stop()
		run.send(resultMsg{op: "initialize", value: &rpc.InitializeResult{UserAgent: "dalgona/test", PlatformFamily: "unix"}})
		run.send(commandDone("timeout-proof"))
		run.waitFor(t, "bottom-anchor query armed", func(string, string) bool {
			state := run.read(func(m model) any { return *m.anchorState() }).(dockAnchor)
			return state.querying && state.needed
		})
		run.waitFor(t, "unanswered query abandoned", func(string, string) bool {
			state := run.read(func(m model) any { return *m.anchorState() }).(dockAnchor)
			return !state.querying && !state.needed
		})
		if got := strings.Count(run.raw(), ansi.RequestCursorPositionReport); got != 1 {
			t.Fatalf("cursor queries on a mute terminal = %d, want 1", got)
		}
		run.send(commandDone("after-timeout"))
		run.waitFor(t, "printing continues after timeout", func(history, screen string) bool {
			return rowCount(history, screen, "after-timeout") == 1
		})
		if got := strings.Count(run.raw(), ansi.RequestCursorPositionReport); got != 1 {
			t.Fatalf("cursor queries after abandoned arming = %d, want 1", got)
		}
	})
}

func TestDockQueryTimeoutRejectsStaleGeneration(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.screen.width, m.screen.height = 40, 8
	m.out.geometry = m.currentGeometry()
	m.anchorState().needed = true
	if m.anchorDock() == nil {
		t.Fatal("missing cursor query")
	}
	state := m.anchorState()
	m.queryExpired(dockQueryTimeoutMsg{query: state.query + 1})
	if !state.querying || !state.needed {
		t.Fatal("a stale timeout abandoned a live query")
	}
	m.queryExpired(dockQueryTimeoutMsg{query: state.query})
	if state.querying || state.needed {
		t.Fatal("the matching timeout failed to abandon the arming")
	}
	if m.positionDock(tea.CursorPositionMsg{X: 2, Y: 5}) != nil {
		t.Fatal("a reply after the timeout relocated the dock")
	}
}

func TestDockRejectsRowOneReportAsModifiedF3(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.screen.width, m.screen.height = 40, 8
	m.out.geometry = m.currentGeometry()
	m.anchorState().needed = true
	m.anchorState().prints = 1
	if m.anchorDock() == nil {
		t.Fatal("missing cursor query")
	}
	if command := m.positionDock(tea.CursorPositionMsg{X: 2, Y: 0}); command != nil {
		t.Fatal("a modified-F3 signature relocated the dock")
	}
	state := m.anchorState()
	if state.querying {
		t.Fatal("the phantom report left the query in flight")
	}
	if !state.needed {
		t.Fatal("the phantom report abandoned the arming")
	}
	if m.anchorDock() == nil {
		t.Fatal("the dock stopped re-arming after a phantom report")
	}
	if command := m.positionDock(tea.CursorPositionMsg{X: 2, Y: 2}); command == nil {
		t.Fatal("a real cursor report stopped relocating the dock")
	}
}

func agentMessageTurn(text string) []tea.Msg {
	return []tea.Msg{
		fuzzNote("thread/started", invThreadStarted),
		fuzzNote("turn/started", invTurnStarted),
		fuzzNote("item/started", fuzzAgentStarted),
		fuzzDelta("item/agentMessage/delta", "a1", text),
		fuzzNote("item/completed", fuzzAgentDone),
		fuzzNote("turn/completed", `{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"completed"}}`),
	}
}

const tabbedLines = 30

func tabbedFence() string {
	var fence strings.Builder
	fence.WriteString("```go\n")
	for i := range tabbedLines {
		fmt.Fprintf(&fence, "\t\tcall(%02d, x%02d%s)\n", i, i, strings.Repeat("y", 22))
	}
	fence.WriteString("```\n\nAfter the block.\n")
	return fence.String()
}

func tabbedRowsPrintedOnce(t *testing.T, history, screen string) {
	t.Helper()
	for i := range tabbedLines {
		for _, part := range []string{fmt.Sprintf("call(%02d,", i), fmt.Sprintf("x%02dyyyy", i)} {
			if got := rowCount(history, screen, part); got != 1 {
				t.Fatalf("%q appears %d times, want once:\nhistory:\n%s\nscreen:\n%s", part, got, history, screen)
			}
		}
	}
}

func TestTabbedCodeRowsKeepDockOutOfHistory(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		em := vt.NewEmulator(40, 12)
		tmuxMode(em)
		run := startEmulatedProgram(em, em, nil)
		defer run.stop()
		run.send(resultMsg{op: "initialize", value: &rpc.InitializeResult{UserAgent: "dalgona/test", PlatformFamily: "unix"}})
		run.send(commandDone("anchor-turn"))
		bandAtBottom(t, run, em, 12, "startup before any input")
		for _, msg := range agentMessageTurn(tabbedFence()) {
			run.send(msg)
		}
		run.waitFor(t, "text after the block printed", func(history, screen string) bool {
			return rowCount(history, screen, "After the block.") == 1
		})
		bandAtBottom(t, run, em, 12, "after tabbed code rows")
		history := run.history()
		if line := leakedLine(history, run.dock()); line != "" {
			t.Fatalf("dock line %q in history:\n%s", line, history)
		}
		tabbedRowsPrintedOnce(t, history, run.screen())
		rawOutputInvariant(t, run)
	})
}
