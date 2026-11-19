package tui

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/vt"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	diagramFence = "```d2\nx -> y\n```\n"
	diagramArt   = "ART-ONE\nART-TWO"
	diagramCard  = "diagram d2 · 2 rows"
	afterText    = "After the diagram."

	diagramRunWidth = 100
)

func blockedDraw(gate <-chan struct{}, art string) drawFunc {
	return func(context.Context, string, string) (string, error) {
		<-gate
		return art, nil
	}
}

func agentStarted(id string) notificationMsg {
	return notificationMsg{method: "item/started", params: []byte(`{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"agentMessage","id":"` + id + `","text":""}}`)}
}

func agentDelta(id, delta string) notificationMsg {
	quoted, err := jsontext.AppendQuote(nil, delta)
	if err != nil {
		panic(err)
	}
	return notificationMsg{method: "item/agentMessage/delta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"` + id + `","delta":` + string(quoted) + `}`)}
}

type diagramRun struct {
	*emulatedRun
	gate    chan struct{}
	release func()
	feed    chan tea.Msg
}

func startDiagramRun(t *testing.T, draw func(gate <-chan struct{}) drawFunc) *diagramRun {
	r := &diagramRun{emulatedRun: startEmulatedProgram(vt.NewEmulator(diagramRunWidth, 24), nil, nil), gate: make(chan struct{}), feed: make(chan tea.Msg, 64)}
	r.release = sync.OnceFunc(func() { close(r.gate) })
	go func() {
		for msg := range r.feed {
			r.emulatedRun.send(msg)
		}
	}()
	r.send(func(m *model) { m.out.draw = draw(r.gate) })
	r.send(commandDone("seq 3"))
	r.waitFor(t, "the first row printed", func(h, s string) bool { return rowCount(h, s, "seq 3") == 1 })
	r.send(agentStarted("a1"))
	return r
}

func (r *diagramRun) send(msg tea.Msg) {
	r.feed <- msg
}

func (r *diagramRun) finish() {
	r.release()
	close(r.feed)
	r.stop()
}

func (r *diagramRun) quiet() {
	synctest.Wait()
}

func gatedRun(t *testing.T, art string) *diagramRun {
	t.Helper()
	return startDiagramRun(t, func(gate <-chan struct{}) drawFunc { return blockedDraw(gate, art) })
}

func printedOrder(t *testing.T, history, screen string, wants ...string) {
	t.Helper()
	all := history + "\n" + screen
	last := -1
	for _, want := range wants {
		if n := strings.Count(all, want); n != 1 {
			t.Fatalf("%q printed %d times, want once:\n%s", want, n, all)
		}
		at := strings.Index(all, want)
		if at < last {
			t.Fatalf("%q printed before the row it must follow:\n%s", want, all)
		}
		last = at
	}
}

func TestKeyHandledWhileDiagramRenders(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		run := gatedRun(t, diagramArt)
		defer run.finish()
		run.send(agentDelta("a1", diagramFence))
		run.quiet()
		run.send(press('k', 0, "k"))
		run.waitFor(t, "the key echoed while the renderer has not returned", func(_, s string) bool {
			return strings.Contains(s, "› k")
		})
		if all := run.history() + run.screen(); strings.Contains(all, "ART-ONE") {
			t.Fatalf("the card printed before the renderer returned:\n%s", all)
		}
		run.release()
		run.waitFor(t, "the card printed after the renderer returned", func(h, s string) bool {
			return strings.Contains(h+s, "ART-TWO")
		})
		printedOrder(t, run.history(), run.screen(), diagramCard, "ART-ONE", "ART-TWO")
	})
}

func TestRowsAfterDiagramWaitForIt(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		run := gatedRun(t, diagramArt)
		defer run.finish()
		run.send(agentDelta("a1", diagramFence))
		run.send(agentDelta("a1", afterText+"\n\n"))
		run.quiet()
		run.send(press('k', 0, "k"))
		run.waitFor(t, "the key echoed while a row waits behind the renderer", func(_, s string) bool {
			return strings.Contains(s, "› k")
		})
		if all := run.history() + run.screen(); strings.Contains(all, afterText) {
			t.Fatalf("the row after the diagram printed before the diagram:\n%s", all)
		}
		run.release()
		run.waitFor(t, "the diagram and the row after it printed", func(h, s string) bool {
			return strings.Contains(h+s, afterText) && strings.Contains(h+s, "ART-TWO")
		})
		printedOrder(t, run.history(), run.screen(), diagramCard, "ART-TWO", afterText)
	})
}

func TestDiagramTimeoutPrintsSource(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		run := gatedRun(t, diagramArt)
		defer run.finish()
		run.send(agentDelta("a1", diagramFence))
		run.quiet()
		time.Sleep(diagramBudget + time.Second)
		run.quiet()
		run.waitFor(t, "the source printed after the budget", func(h, s string) bool {
			return strings.Contains(h+s, "diagram d2: render failed · source shown") && strings.Contains(h+s, "x -> y")
		})
		printedOrder(t, run.history(), run.screen(), "diagram d2: render failed · source shown", "x -> y")
		if all := run.history() + run.screen(); strings.Contains(all, "ART-ONE") {
			t.Fatalf("a timed out render printed art:\n%s", all)
		}
	})
}

func TestQuitFlushesRowsHeldBehindDiagram(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		run := gatedRun(t, diagramArt)
		defer run.finish()
		run.send(agentDelta("a1", diagramFence))
		run.send(agentDelta("a1", afterText+"\n\n"))
		run.quiet()
		run.send(press('c', tea.ModCtrl, ""))
		run.send(press('c', tea.ModCtrl, ""))
		run.quiet()
		select {
		case <-run.done:
			t.Fatal("quit returned while the diagram was still rendering")
		default:
		}
		time.Sleep(diagramBudget + time.Second)
		run.quiet()
		select {
		case <-run.done:
		default:
			t.Fatal("quit did not return within the diagram budget")
		}
		printedOrder(t, run.history(), run.screen(), "diagram d2: render failed · source shown", afterText, "--resume t1")
	})
}

func suspendModel(t *testing.T, gate <-chan struct{}) model {
	t.Helper()
	m := newTestModel(new(fakeCaller), Options{})
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m = rawUpdate(t, m, reviewThreadStarted())
	m = rawUpdate(t, m, commandOutput("seq", numberedOutput(3)))
	m = settleFrames(m)
	m.out.draw = blockedDraw(gate, diagramArt)
	m = rawUpdate(t, m, agentStarted("a1"))
	return settleFrames(m)
}

func updateUnblocked(t *testing.T, m model, msg tea.Msg) (model, tea.Cmd) {
	t.Helper()
	type outcome struct {
		m       model
		command tea.Cmd
	}
	done := make(chan outcome, 1)
	go func() {
		next, command := m.Update(msg)
		done <- outcome{m: next.(model), command: command}
	}()
	synctest.Wait()
	select {
	case out := <-done:
		return out.m, out.command
	default:
		t.Fatal("Update blocked in the renderer")
		return m, nil
	}
}

func runDiagramCommands(command tea.Cmd, results chan<- diagramMsg) {
	if command == nil {
		return
	}
	go func() {
		msg := command()
		if batch, ok := msg.(tea.BatchMsg); ok {
			for _, next := range batch {
				runDiagramCommands(next, results)
			}
			return
		}
		if result, ok := msg.(diagramMsg); ok {
			results <- result
		}
	}()
}

func takeDiagramResult(t *testing.T, results <-chan diagramMsg) diagramMsg {
	t.Helper()
	select {
	case result := <-results:
		return result
	default:
		t.Fatal("the diagram command returned no result")
		return diagramMsg{}
	}
}

func pumpUntilSuspend(t *testing.T, m model, command tea.Cmd) (model, bool) {
	t.Helper()
	for range 20 {
		if hasMsg[tea.SuspendMsg](commandMsgs(command)) {
			return m, true
		}
		var msg tea.Msg
		switch {
		case m.out.settling:
			msg = settledMsg{n: m.out.settleN}
		case m.out.printing:
			msg = printedMsg{}
		default:
			t.Fatalf("suspend still waits with nothing in flight: pending %q", m.out.pending)
		}
		m, command = updateUnblocked(t, m, msg)
	}
	return m, false
}

func TestSuspendFlushesRowsHeldBehindDiagram(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		gate := make(chan struct{})
		release := sync.OnceFunc(func() { close(gate) })
		defer func() {
			release()
			time.Sleep(time.Minute)
		}()
		results := make(chan diagramMsg, 8)
		m := suspendModel(t, gate)
		var command tea.Cmd
		m, command = updateUnblocked(t, m, agentDelta("a1", diagramFence))
		runDiagramCommands(command, results)
		m, command = updateUnblocked(t, m, agentDelta("a1", afterText+"\n\n"))
		runDiagramCommands(command, results)
		prints := m.anchorState().prints
		m, command = updateUnblocked(t, m, press('z', tea.ModCtrl, ""))
		if !m.suspending || hasMsg[tea.SuspendMsg](commandMsgs(command)) {
			t.Fatalf("suspend returned while the diagram was rendering: suspending %t", m.suspending)
		}
		time.Sleep(diagramBudget + time.Second)
		synctest.Wait()
		m, command = updateUnblocked(t, m, takeDiagramResult(t, results))
		m, suspended := pumpUntilSuspend(t, m, command)
		if !suspended {
			t.Fatal("suspend never returned after the held rows printed")
		}
		if m.anchorState().prints == prints || len(m.out.pending) != 0 || m.out.printing {
			t.Fatalf("suspend returned before the held rows printed: prints %d to %d, pending %q, printing %t", prints, m.anchorState().prints, m.out.pending, m.out.printing)
		}
	})
}

func reviewWaiting(t *testing.T, m model) string {
	t.Helper()
	if m.review == nil {
		t.Fatal("the review is closed")
	}
	for line := range strings.SplitSeq(stripANSI(m.View().Content), "\n") {
		if strings.HasPrefix(line, "Review · ") {
			return line
		}
	}
	t.Fatalf("no review header in:\n%s", m.View().Content)
	return ""
}

func TestReviewCountsRowsHeldBehindDiagram(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		gate := make(chan struct{})
		release := sync.OnceFunc(func() { close(gate) })
		defer func() {
			release()
			time.Sleep(time.Minute)
		}()
		results := make(chan diagramMsg, 8)
		m := suspendModel(t, gate)
		var command tea.Cmd
		m, command = updateUnblocked(t, m, agentDelta("a1", diagramFence))
		runDiagramCommands(command, results)
		m, _ = updateUnblocked(t, m, press('o', tea.ModCtrl, ""))
		m, _ = updateUnblocked(t, m, press('[', 0, "["))
		m.commit("probe:held", []string{"held row"})
		if header := reviewWaiting(t, m); !strings.Contains(header, "· 1 waiting") {
			t.Fatalf("review header %q does not count the row held behind the diagram", header)
		}
		if view := stripANSI(m.View().Content); !strings.Contains(view, "diagram d2 · drawing") || !strings.Contains(view, "x -> y") {
			t.Fatalf("the review does not show the entry of the diagram that renders followed by its source:\n%s", view)
		}
		release()
		synctest.Wait()
		m, _ = updateUnblocked(t, m, takeDiagramResult(t, results))
		if header := reviewWaiting(t, m); !strings.Contains(header, "· 4 waiting") {
			t.Fatalf("review header %q does not count the card rows and the held row", header)
		}
		if view := stripANSI(m.View().Content); !strings.Contains(view, "ART-ONE") || strings.Contains(view, "drawing") || strings.Contains(view, "x -> y") {
			t.Fatalf("the open review still shows the entry from before the render ended:\n%s", view)
		}
	})
}

func TestDiagramResultAfterThreadSwitchPrintsOnce(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		run := gatedRun(t, diagramArt)
		defer run.finish()
		run.send(agentDelta("a1", diagramFence))
		run.quiet()
		other := rpc.Thread{ID: "t2", Name: "other", Turns: []rpc.Turn{{ID: "turn-9", Status: "completed", Items: []rpc.Item{{Type: "commandExecution", ID: "c-other", Command: "echo other", Status: "completed", Output: "other-output"}}}}}
		run.send(resultMsg{op: "resumed", value: &resumedThread{Thread: other}})
		run.quiet()
		run.send(press('k', 0, "k"))
		run.waitFor(t, "the key echoed after the thread switch", func(_, s string) bool {
			return strings.Contains(s, "› k")
		})
		if all := run.history() + run.screen(); strings.Contains(all, "other-output") {
			t.Fatalf("the other thread printed before the diagram of the first thread:\n%s", all)
		}
		run.release()
		run.waitFor(t, "the card and the other thread printed", func(h, s string) bool {
			return strings.Contains(h+s, "ART-TWO") && strings.Contains(h+s, "other-output")
		})
		run.send(diagramMsg{key: "block:t1:a1:0", lines: []string{"STALE-DUPLICATE"}})
		run.quiet()
		time.Sleep(time.Second)
		run.quiet()
		printedOrder(t, run.history(), run.screen(), diagramCard, "ART-TWO", "other-output")
		if all := run.history() + run.screen(); strings.Contains(all, "STALE-DUPLICATE") {
			t.Fatalf("a second result for one diagram printed:\n%s", all)
		}
	})
}

func TestDiagramSourceOverLimitIsNotDrawn(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name  string
		bytes int
		drawn int32
	}{
		{"at the limit", 16 << 10, 1},
		{"over the limit", 16<<10 + 1, 0},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				var calls atomic.Int32
				run := startDiagramRun(t, func(<-chan struct{}) drawFunc {
					return func(context.Context, string, string) (string, error) {
						calls.Add(1)
						return diagramArt, nil
					}
				})
				defer run.finish()
				source := "# " + strings.Repeat("a", c.bytes-2)
				run.send(agentDelta("a1", "```d2\n"+source+"\n```\n"))
				run.quiet()
				time.Sleep(time.Second)
				run.quiet()
				text := run.read(func(m model) any {
					var all []string
					for _, entry := range m.out.entries {
						all = append(all, entryTitle(entry.text))
					}
					return strings.Join(all, "\n")
				}).(string)
				if got := calls.Load(); got != c.drawn {
					t.Fatalf("the renderer ran %d times for a %d byte source, want %d", got, c.bytes, c.drawn)
				}
				refused := strings.Contains(text, "diagram d2: too large to draw · source shown")
				if refused != (c.drawn == 0) {
					t.Fatalf("too large card shown %t for a %d byte source:\n%s", refused, c.bytes, text)
				}
			})
		})
	}
}

func TestDiagramDrawPanicPrintsSource(t *testing.T) {
	t.Parallel()
	lines := diagramLines(func(context.Context, string, string) (string, error) { panic("boom") }, block{text: "x -> y", diagram: "d2"})
	want := []string{"diagram d2: render failed · source shown", "x -> y"}
	if !slices.Equal(lines, want) {
		t.Fatalf("a renderer that panics printed %q, want %q", lines, want)
	}
}

func TestRenderD2SkipsWorkAfterBudget(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	art, err := renderD2(ctx, "x -> y\n")
	if !errors.Is(err, context.Canceled) || art != "" {
		t.Fatalf("renderD2 with a spent budget = (%q, %v), want a context error and no art", art, err)
	}
}

func agentCompleted(id, text string) notificationMsg {
	quoted, err := jsontext.AppendQuote(nil, text)
	if err != nil {
		panic(err)
	}
	return notificationMsg{method: "item/completed", params: []byte(`{"threadId":"t1","turnId":"turn-1","completedAtMs":2,"item":{"type":"agentMessage","id":"` + id + `","text":` + string(quoted) + `}}`)}
}

func TestDiagramRedeliveredItemPrintsOnce(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		var calls atomic.Int32
		run := startDiagramRun(t, func(gate <-chan struct{}) drawFunc {
			return func(ctx context.Context, kind, source string) (string, error) {
				calls.Add(1)
				return blockedDraw(gate, diagramArt)(ctx, kind, source)
			}
		})
		defer run.finish()
		run.send(agentCompleted("a1", diagramFence))
		run.send(agentCompleted("a1", diagramFence))
		run.quiet()
		run.release()
		run.waitFor(t, "the card printed", func(h, s string) bool {
			return strings.Contains(h+s, "ART-TWO")
		})
		run.quiet()
		time.Sleep(time.Second)
		run.quiet()
		printedOrder(t, run.history(), run.screen(), diagramCard, "ART-ONE", "ART-TWO")
		if got := calls.Load(); got != 1 {
			t.Fatalf("a redelivered item rendered its diagram %d times, want 1", got)
		}
	})
}

func TestDiagramsPrintInCommitOrder(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		first, second := make(chan struct{}), make(chan struct{})
		gates := map[string]chan struct{}{"x -> y": first, "a -> b": second}
		arts := map[string]string{"x -> y": "ART-FIRST", "a -> b": "ART-SECOND"}
		run := startDiagramRun(t, func(<-chan struct{}) drawFunc {
			return func(_ context.Context, _, source string) (string, error) {
				source = strings.TrimSpace(source)
				<-gates[source]
				return arts[source], nil
			}
		})
		releaseFirst, releaseSecond := sync.OnceFunc(func() { close(first) }), sync.OnceFunc(func() { close(second) })
		defer run.finish()
		defer releaseFirst()
		defer releaseSecond()
		run.send(agentDelta("a1", "```d2\nx -> y\n```\nMIDDLE\n\n```d2\na -> b\n```\nTAIL\n\n"))
		run.quiet()
		releaseSecond()
		run.quiet()
		time.Sleep(time.Second)
		run.quiet()
		if all := run.history() + run.screen(); strings.Contains(all, "ART-SECOND") || strings.Contains(all, "MIDDLE") || strings.Contains(all, "TAIL") {
			t.Fatalf("a result printed before the diagram that precedes it:\n%s", all)
		}
		releaseFirst()
		run.waitFor(t, "every row printed", func(h, s string) bool {
			return strings.Contains(h+s, "TAIL") && strings.Contains(h+s, "ART-SECOND")
		})
		printedOrder(t, run.history(), run.screen(), "ART-FIRST", "MIDDLE", "ART-SECOND", "TAIL")
	})
}
