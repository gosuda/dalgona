package main

import (
	"context"
	"io"
	"log/slog"
	"os"
	"slices"
	"sync"
	"time"

	"github.com/gosuda/dalgona/cmd/internal/tuiwire/dalgonawire"
	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/herdr"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/registry"
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
	dial, closeSession := headlessDial(cfg, std)
	defer closeSession()
	initial := std.remoteName(cfg.Remote)
	openRemotes := func() (*client.Remotes, error) {
		store, err := authStore(cfg)
		if err != nil {
			return nil, err
		}
		logger := slog.New(slog.NewTextHandler(std.errOut, nil))
		services := registry.New(registry.Options{Auth: store, Logger: logger})
		return client.NewRemotes(cfg, services), nil
	}
	env := app.CLIEnv{
		Product: config.ProductDalgona, Home: cfg.Home, Remote: initial,
		Dial: dial, Stdin: std.in, Stdout: std.out, Stderr: std.errOut,
		TUI: dalgonawire.Launch(cfg, initial, openRemotes),
	}
	release := attachHerdr(ctx, &env, cfg.Herdr, os.Getenv)
	code := app.RunCLI(ctx, env, args)
	release()
	return code
}

func headlessDial(cfg config.Config, std stdio) (func(context.Context) (io.ReadWriteCloser, error), func()) {
	var session *client.Session
	open := sync.OnceValues(func() (*client.Session, error) {
		var err error
		session, err = openSession(cfg, std)
		return session, err
	})
	return func(ctx context.Context) (io.ReadWriteCloser, error) {
			opened, err := open()
			if err != nil {
				return nil, err
			}
			return connect(ctx, opened)
		}, func() {
			if session != nil {
				_ = session.Close()
			}
		}
}

type herdrWorker struct {
	reporter *herdr.Reporter
	wake     chan struct{}
	done     chan struct{}
	mu       sync.Mutex
	closed   bool
	last     app.Status
	pending  map[[2]string]app.Status
	order    [][2]string
	panes    [][2]string
}

func attachHerdr(ctx context.Context, env *app.CLIEnv, cfg config.HerdrConfig, getenv func(string) string) func() {
	reporter := herdr.FromEnv(getenv)
	if reporter == nil {
		return func() {}
	}
	w := &herdrWorker{
		reporter: reporter, wake: make(chan struct{}, 1), done: make(chan struct{}),
		pending: make(map[[2]string]app.Status),
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

func (w *herdrWorker) status(status app.Status) {
	if status.ThreadID == "" {
		return
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	sameThread := status.Endpoint == w.last.Endpoint && status.ThreadID == w.last.ThreadID
	if w.closed || (sameThread && (status.Seq < w.last.Seq || status == w.last)) {
		return
	}
	w.last = status
	key := [2]string{status.Endpoint, status.ThreadID}
	if pos := slices.Index(w.order, key); pos >= 0 {
		copy(w.order[pos:], w.order[pos+1:])
		w.order[len(w.order)-1] = key
	} else {
		w.order = append(w.order, key)
	}
	w.pending[key] = status
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
			w.reporter.Report(ctx, herdrState(status.State), status.Seq, status.Remote, status.ThreadID)
			cancel()
		}
		for _, pane := range panes {
			ctx, cancel := context.WithTimeout(base, herdrCallTimeout)
			w.reporter.SubagentPane(ctx, pane[0], pane[1])
			cancel()
		}
	}
}

func (w *herdrWorker) take() ([]app.Status, [][2]string) {
	w.mu.Lock()
	defer w.mu.Unlock()
	statuses := make([]app.Status, 0, len(w.order))
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
