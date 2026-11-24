package tui

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/charmbracelet/x/vt"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const caretReturnBudget = 100 * time.Millisecond

func emulatedCursor(run *emulatedRun) (x, y int) {
	run.mu.Lock()
	defer run.mu.Unlock()
	cursor := run.em.CursorPosition()
	return cursor.X, cursor.Y
}

func startInitialized(width, height int) (*vt.Emulator, *emulatedRun) {
	em := vt.NewEmulator(width, height)
	tmuxMode(em)
	run := startEmulatedProgram(em, em, nil)
	run.send(resultMsg{op: "initialize", value: &rpc.InitializeResult{UserAgent: "dalgona/test", PlatformFamily: "unix"}})
	return em, run
}

func assertNoScreenReset(t *testing.T, run *emulatedRun) {
	t.Helper()
	for _, forbidden := range []string{"\x1b[2J", "\x1b[3J", "\x1b[H"} {
		if strings.Contains(run.raw(), forbidden) {
			t.Fatalf("forbidden sequence %q in output:\n%q", forbidden, run.raw())
		}
	}
}

func assertCursorAt(t *testing.T, run *emulatedRun, x, y int, when string) {
	t.Helper()
	if gotX, gotY := emulatedCursor(run); gotX != x || gotY != y {
		t.Fatalf("cursor %s = (%d,%d), want the caret (%d,%d):\n%s", when, gotX, gotY, x, y, run.screen())
	}
}

func TestCursorReturnsToCaretAfterCommittedPrint(t *testing.T) {
	t.Parallel()
	for _, draft := range []string{"", "ab", "hello world"} {
		t.Run(fmt.Sprintf("draft %q", draft), func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				const width, height = 40, 12
				_, run := startInitialized(width, height)
				defer run.stop()
				run.send(commandDone("caret-first"))
				run.waitFor(t, "first row printed", func(h, s string) bool { return rowCount(h, s, "caret-first") == 1 })
				if draft != "" {
					run.send(tea.KeyPressMsg{Code: rune(draft[0]), Text: draft})
				}
				time.Sleep(time.Second)
				synctest.Wait()
				wantX, wantY := 2+len(draft), height-3
				assertCursorAt(t, run, wantX, wantY, "before the second print")
				run.send(commandDone("caret-second"))
				run.waitFor(t, "second row printed", func(h, s string) bool { return rowCount(h, s, "caret-second") == 1 })
				time.Sleep(caretReturnBudget)
				synctest.Wait()
				assertCursorAt(t, run, wantX, wantY, fmt.Sprintf("%v after a committed print", caretReturnBudget))
				time.Sleep(3 * time.Second)
				synctest.Wait()
				assertCursorAt(t, run, wantX, wantY, "while idle")
				assertNoScreenReset(t, run)
			})
		})
	}
}

type closedPanel struct {
	name  string
	open  func(run *emulatedRun)
	close func(run *emulatedRun)
}

func closedPanels() []closedPanel {
	const single = `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"Database","question":"Which database?","isOther":true,"isSecret":false,"multiSelect":false,` +
		`"options":[{"label":"SQLite","description":"One file."},{"label":"Postgres","description":"A server."}]}]}`
	return []closedPanel{
		{
			name:  "approval declined",
			open:  func(run *emulatedRun) { run.send(approvalRequest("make all")) },
			close: func(run *emulatedRun) { run.send(press('n', 0, "n")) },
		},
		{
			name: "question submitted",
			open: func(run *emulatedRun) {
				run.send(questionRequest(make(chan requestAnswer, 1), single))
			},
			close: func(run *emulatedRun) {
				run.send(press('2', 0, "2"))
				run.send(press(tea.KeyEnter, 0, ""))
				run.send(requestResolved("t1", "req-1", "answered"))
			},
		},
	}
}

func TestDockReachesBottomAfterPanelClosesAndRowsPrint(t *testing.T) {
	t.Parallel()
	sizes := []tea.WindowSizeMsg{{Width: 100, Height: 30}, {Width: 80, Height: 24}, {Width: 40, Height: 12}}
	for _, panel := range closedPanels() {
		for _, size := range sizes {
			t.Run(fmt.Sprintf("%s at %dx%d", panel.name, size.Width, size.Height), func(t *testing.T) {
				t.Parallel()
				synctest.Test(t, func(t *testing.T) {
					em, run := startInitialized(size.Width, size.Height)
					defer run.stop()
					run.send(commandDone("panel-first"))
					run.waitFor(t, "first row printed", func(h, s string) bool { return rowCount(h, s, "panel-first") == 1 })
					bandAtBottom(t, run, em, size.Height, "start")
					panel.open(run)
					run.waitFor(t, "panel open", func(_, s string) bool {
						return strings.Contains(ansi.Strip(s), "Which database?") || strings.Contains(ansi.Strip(s), "Approval")
					})
					time.Sleep(time.Second)
					synctest.Wait()
					panel.close(run)
					run.send(func(m *model) { m.setState(stateIdle) })
					run.send(commandDone("panel-closed"))
					run.waitFor(t, "row after the panel printed", func(h, s string) bool { return rowCount(h, s, "panel-closed") == 1 })
					time.Sleep(time.Second)
					synctest.Wait()
					bandAtBottom(t, run, em, size.Height, "after the panel closed")
					assertNoScreenReset(t, run)
				})
			})
		}
	}
}

func TestIdleDockAfterPrintSendsNoCursorQueryLoop(t *testing.T) {
	t.Parallel()
	for _, panel := range closedPanels() {
		t.Run(panel.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				const idle = 5
				_, run := startInitialized(100, 30)
				defer run.stop()
				run.send(commandDone("loop-first"))
				run.waitFor(t, "first row printed", func(h, s string) bool { return rowCount(h, s, "loop-first") == 1 })
				panel.open(run)
				time.Sleep(time.Second)
				synctest.Wait()
				panel.close(run)
				run.send(func(m *model) { m.setState(stateIdle) })
				run.send(commandDone("loop-closed"))
				run.waitFor(t, "row after the panel printed", func(h, s string) bool { return rowCount(h, s, "loop-closed") == 1 })
				time.Sleep(time.Second)
				synctest.Wait()
				before := strings.Count(run.raw(), ansi.RequestCursorPositionReport)
				time.Sleep(idle * time.Second)
				synctest.Wait()
				if got := strings.Count(run.raw(), ansi.RequestCursorPositionReport) - before; got > idle {
					t.Fatalf("%d cursor queries in %d idle seconds, want at most %d", got, idle, idle)
				}
			})
		})
	}
}

func commitRows(prefix string, count int) func(m *model) {
	return func(m *model) {
		for i := 1; i <= count; i++ {
			m.commit(fmt.Sprintf("%s:%d", prefix, i), []string{fmt.Sprintf("%s-%02d", prefix, i)})
		}
	}
}

func TestCursorOnCaretAfterRowsPrintedInSeveralChunks(t *testing.T) {
	t.Parallel()
	const width, height = 40, 8
	bound := height - 3
	for _, chunks := range []int{2, 3, 4} {
		t.Run(fmt.Sprintf("%d chunks", chunks), func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				_, run := startInitialized(width, height)
				defer run.stop()
				run.send(commandDone("chunk-first"))
				run.waitFor(t, "first row printed", func(h, s string) bool { return rowCount(h, s, "chunk-first") == 1 })
				time.Sleep(time.Second)
				synctest.Wait()
				rows := chunks * bound
				run.send(commitRows("chunk", rows))
				run.waitFor(t, "every chunk printed", func(h, s string) bool {
					return rowCount(h, s, fmt.Sprintf("chunk-%02d", rows)) == 1
				})
				time.Sleep(caretReturnBudget)
				synctest.Wait()
				if x, y := emulatedCursor(run); x != 2 || y != height-3 {
					t.Fatalf("cursor %v after %d chunks = (%d,%d), want the caret (2,%d):\n%s", caretReturnBudget, chunks, x, y, height-3, run.screen())
				}
				assertNoScreenReset(t, run)
			})
		})
	}
}

func TestStartupInTinyWindowPutsCursorOnCaretWithoutQueryLoop(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		const width, height, idle = 20, 6, 5
		_, run := startInitialized(width, height)
		defer run.stop()
		run.send(commitRows("tiny", 7))
		run.waitFor(t, "welcome rows printed", func(h, s string) bool { return rowCount(h, s, "tiny-07") == 1 })
		time.Sleep(time.Second)
		synctest.Wait()
		before := strings.Count(run.raw(), ansi.RequestCursorPositionReport)
		time.Sleep(idle * time.Second)
		synctest.Wait()
		if got := strings.Count(run.raw(), ansi.RequestCursorPositionReport) - before; got > idle {
			t.Fatalf("%d cursor queries in %d idle seconds at %dx%d, want at most %d", got, idle, width, height, idle)
		}
		if x, y := emulatedCursor(run); x != 2 || y != height-3 {
			t.Fatalf("cursor = (%d,%d) at %dx%d, want the caret (2,%d):\n%s", x, y, width, height, height-3, run.screen())
		}
	})
}

func TestDockMovedAfterComposerShrinksMatchesCleanPaint(t *testing.T) {
	t.Parallel()
	for _, size := range []tea.WindowSizeMsg{{Width: 100, Height: 30}, {Width: 80, Height: 24}, {Width: 40, Height: 12}} {
		t.Run(fmt.Sprintf("%dx%d", size.Width, size.Height), func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				_, run := startInitialized(size.Width, size.Height)
				defer run.stop()
				run.send(commandDone("shrink-first"))
				run.waitFor(t, "first row printed", func(h, s string) bool { return rowCount(h, s, "shrink-first") == 1 })
				time.Sleep(time.Second)
				synctest.Wait()
				run.send(func(m *model) { m.input.composer.SetValue("S7MARK ") })
				run.send(tea.PasteMsg{Content: numberedOutput(30)})
				time.Sleep(time.Second)
				synctest.Wait()
				tall := run.dock()
				run.send(press('c', tea.ModCtrl, ""))
				time.Sleep(3 * time.Second)
				synctest.Wait()
				dock := run.dock()
				if len(dock) >= len(tall) {
					t.Fatalf("dock did not shrink: %d rows before, %d after", len(tall), len(dock))
				}
				for i, line := range dock {
					want := strings.TrimRight(ansi.Strip(line), " ")
					got := strings.TrimRight(run.physicalRow(size.Height-len(dock)+i), " ")
					if got != want {
						t.Fatalf("dock row %d reads %q, want %q:\n%s", i, got, want, run.screen())
					}
				}
				assertNoScreenReset(t, run)
			})
		})
	}
}

func TestDockMovedAtStartupLeavesNoCellOfTheBandMark(t *testing.T) {
	t.Parallel()
	for _, top := range []int{4, 5, 6} {
		t.Run(fmt.Sprintf("cursor starts at row %d", top), func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				const width, height = 40, 12
				em := vt.NewEmulator(width, height)
				tmuxMode(em)
				_, _ = fmt.Fprintf(em, "\x1b[%d;1H", top+1)
				run := startEmulatedProgram(em, em, nil)
				defer run.stop()
				run.send(resultMsg{op: "initialize", value: &rpc.InitializeResult{UserAgent: "dalgona/test", PlatformFamily: "unix"}})
				time.Sleep(2 * time.Second)
				synctest.Wait()
				dock := run.dock()
				for i, line := range dock {
					want := strings.TrimRight(ansi.Strip(line), " ")
					got := strings.TrimRight(run.physicalRow(height-len(dock)+i), " ")
					if got != want {
						t.Fatalf("dock row %d reads %q, want %q:\n%s", i, got, want, run.screen())
					}
				}
				assertNoScreenReset(t, run)
			})
		})
	}
}

var errDockWrite = errors.New("dock write failed")

type failingDockWrite struct {
	wire   io.Writer
	mu     sync.Mutex
	failed bool
	at     time.Time
}

func (w *failingDockWrite) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if !w.failed && bytes.Contains(p, []byte(ansi.EraseEntireLine+ansi.CursorDown(1)+ansi.EraseEntireLine)) {
		w.failed = true
		w.at = time.Now()
		return 0, errDockWrite
	}
	return w.wire.Write(p)
}

func (w *failingDockWrite) failure() (time.Time, bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.at, w.failed
}

func startWithFailedDockMove(t *testing.T, width, height int) (*vt.Emulator, *emulatedRun, time.Time) {
	t.Helper()
	em := vt.NewEmulator(width, height)
	tmuxMode(em)
	_, _ = fmt.Fprintf(em, "\x1b[%d;1H", 5)
	failing := new(failingDockWrite)
	run := startEmulatedThrough(em, em, nil, func(wire io.Writer) io.Writer {
		failing.wire = wire
		return failing
	})
	run.send(resultMsg{op: "initialize", value: &rpc.InitializeResult{UserAgent: "dalgona/test", PlatformFamily: "unix"}})
	run.waitFor(t, "the first dock move to fail", func(string, string) bool {
		_, failed := failing.failure()
		return failed
	})
	at, _ := failing.failure()
	return em, run, at
}

func TestQuitReturnsAfterFailedDockWrite(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		_, run, failedAt := startWithFailedDockMove(t, 40, 12)
		defer run.stop()
		time.Sleep(500 * time.Millisecond)
		synctest.Wait()
		asked := time.Now()
		run.send(press('c', tea.ModCtrl, ""))
		run.send(press('c', tea.ModCtrl, ""))
		time.Sleep(time.Second)
		synctest.Wait()
		select {
		case <-run.done:
		default:
			moving := run.read(func(m model) any { return m.anchorState().moving }).(bool)
			t.Fatalf("the program still runs %v after the second Ctrl+C and %v after the failed dock write; moving = %v", time.Since(asked), time.Since(failedAt), moving)
		}
	})
}

func TestQuitWaitsForAnUnacknowledgedDockMoveForOneSecond(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		_, run, _ := startWithFailedDockMove(t, 40, 12)
		defer drainHint()
		defer run.stop()
		time.Sleep(800 * time.Millisecond)
		synctest.Wait()
		if !run.read(func(m model) any { return m.anchorState().moving }).(bool) {
			t.Fatal("the dock move was released before one second passed")
		}
		run.send(press('c', tea.ModCtrl, ""))
		run.send(press('c', tea.ModCtrl, ""))
		time.Sleep(100 * time.Millisecond)
		synctest.Wait()
		select {
		case <-run.done:
			t.Fatal("the program returned before the unacknowledged dock move expired")
		default:
		}
		time.Sleep(200 * time.Millisecond)
		synctest.Wait()
		select {
		case <-run.done:
		default:
			t.Fatal("the program still runs after the unacknowledged dock move should have expired")
		}
	})
}

func TestDockReachesBottomAfterFailedDockWrite(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		const width, height = 40, 12
		em, run, _ := startWithFailedDockMove(t, width, height)
		defer run.stop()
		time.Sleep(3 * time.Second)
		synctest.Wait()
		bandAtBottom(t, run, em, height, "a dock write that failed once")
		assertNoScreenReset(t, run)
	})
}
