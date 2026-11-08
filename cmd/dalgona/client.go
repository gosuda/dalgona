package main

import (
	"cmp"
	"context"
	"io"
	"log/slog"
	"os"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/app"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/herdr"
	"github.com/gosuda/dalgona/internal/tui"
)

const (
	herdrQueueSize   = 64
	herdrCallTimeout = 5 * time.Second
)

func runClient(ctx context.Context, args []string, std stdio) int {
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	dial, err := daemonDialer(cfg, std)
	if err != nil {
		return fail(std, 1, err)
	}
	env := app.CLIEnv{
		Product: config.ProductDalgona, Home: cfg.Home, Remote: cmp.Or(std.remote, cfg.Remote),
		Dial:  func(ctx context.Context) (io.ReadWriteCloser, error) { return dial(ctx) },
		Stdin: std.in, Stdout: std.out, Stderr: std.errOut,
	}
	release := attachHerdr(ctx, &env, cfg.Herdr, os.Getenv)
	code := app.RunCLI(ctx, env, args)
	release()
	return code
}

type herdrWorker struct {
	reporter *herdr.Reporter
	wake     chan struct{}
	done     chan struct{}
	mu       sync.Mutex
	closed   bool
	last     tui.Status
	pending  map[string]tui.Status
	order    []string
	panes    [][2]string
}

func attachHerdr(ctx context.Context, env *app.CLIEnv, cfg config.HerdrConfig, getenv func(string) string) func() {
	reporter := herdr.FromEnv(getenv)
	if reporter == nil {
		return func() {}
	}
	w := &herdrWorker{
		reporter: reporter, wake: make(chan struct{}, 1), done: make(chan struct{}),
		pending: make(map[string]tui.Status),
	}
	detached := context.WithoutCancel(ctx)
	work, stopWork := context.WithCancel(detached)
	go w.run(work)
	env.OnStatus = w.status
	if cfg.SubagentPanes {
		env.OnChildSpawned = w.childSpawned
	}
	return func() { w.release(detached, stopWork) }
}

func (w *herdrWorker) status(status tui.Status) {
	if status.ThreadID == "" {
		return
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	sameThread := status.ThreadID == w.last.ThreadID
	if w.closed || (sameThread && (status.Seq < w.last.Seq || status == w.last)) {
		return
	}
	w.last = status
	if _, queued := w.pending[status.ThreadID]; !queued {
		w.order = append(w.order, status.ThreadID)
	}
	w.pending[status.ThreadID] = status
	w.signal()
}

func (w *herdrWorker) childSpawned(childID, cwd string) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.closed || len(w.panes) >= herdrQueueSize {
		return
	}
	w.panes = append(w.panes, [2]string{childID, cwd})
	w.signal()
}

func (w *herdrWorker) signal() {
	select {
	case w.wake <- struct{}{}:
	default:
	}
}

func (w *herdrWorker) run(base context.Context) {
	defer close(w.done)
	for range w.wake {
		statuses, panes := w.take()
		for _, status := range statuses {
			ctx, cancel := context.WithTimeout(base, herdrCallTimeout)
			w.reporter.Report(ctx, herdrState(status.State), status.Seq, status.ThreadID)
			cancel()
		}
		for _, pane := range panes {
			ctx, cancel := context.WithTimeout(base, herdrCallTimeout)
			w.reporter.SubagentPane(ctx, pane[0], pane[1])
			cancel()
		}
	}
}

func (w *herdrWorker) take() ([]tui.Status, [][2]string) {
	w.mu.Lock()
	defer w.mu.Unlock()
	statuses := make([]tui.Status, 0, len(w.order))
	for _, id := range w.order {
		statuses = append(statuses, w.pending[id])
	}
	clear(w.pending)
	panes := w.panes
	w.order, w.panes = nil, nil
	return statuses, panes
}

func (w *herdrWorker) release(base context.Context, stopWork context.CancelFunc) {
	w.mu.Lock()
	if !w.closed {
		w.closed = true
		close(w.wake)
	}
	w.mu.Unlock()
	drain := time.NewTimer(herdrCallTimeout)
	defer drain.Stop()
	select {
	case <-w.done:
	case <-drain.C:
		slog.Warn("herdr: pending reports did not finish before exit")
		stopWork()
		<-w.done
	}
	stopWork()
	ctx, cancel := context.WithTimeout(base, herdrCallTimeout)
	defer cancel()
	w.reporter.Release(ctx)
}

func herdrState(state string) herdr.State {
	switch state {
	case "running":
		return herdr.StateWorking
	case "awaiting approval":
		return herdr.StateBlocked
	default:
		return herdr.StateIdle
	}
}
