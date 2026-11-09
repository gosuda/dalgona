package remote

import (
	"bytes"
	"cmp"
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"log/slog"
	"path"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

var errRunnerClosed = errors.New("remote: runner closed")

type wire struct {
	io.ReadWriteCloser
	dead atomic.Bool
}

func (w *wire) Read(p []byte) (int, error) {
	n, err := w.ReadWriteCloser.Read(p)
	if err != nil {
		w.dead.Store(true)
	}
	return n, err
}

func (w *wire) Write(p []byte) (int, error) {
	n, err := w.ReadWriteCloser.Write(p)
	if err != nil {
		w.dead.Store(true)
	}
	return n, err
}

func (w *wire) Close() error {
	w.dead.Store(true)
	return w.ReadWriteCloser.Close()
}

type link struct {
	remote *runner.Remote
	wire   *wire
	cwd    string
}

func (l *link) close() { _ = l.remote.Close() }

func (l *link) boxPath(p string) string {
	if p == "" {
		return boxWorkspace
	}
	if !filepath.IsAbs(p) || l.cwd == "" {
		return p
	}
	rel, err := filepath.Rel(l.cwd, p)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return p
	}
	return path.Join(boxWorkspace, filepath.ToSlash(rel))
}

type boxRunner struct {
	b       *Binder
	thread  string
	name    string
	spec    config.RunnerSpec
	mu      sync.Mutex
	link    *link
	binding chan struct{}
	closed  bool
}

var _ runner.Runner = (*boxRunner)(nil)

func (*boxRunner) RemoteHost() bool { return true }

func (r *boxRunner) ensure(ctx context.Context) (*link, error) {
	for {
		l, wait, err := r.claim()
		if l != nil || err != nil {
			return l, err
		}
		if wait == nil {
			return r.bindAndPublish(ctx)
		}
		select {
		case <-wait:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
}

func (r *boxRunner) claim() (*link, <-chan struct{}, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	switch {
	case r.closed:
		return nil, nil, errRunnerClosed
	case r.link != nil && !r.link.wire.dead.Load():
		return r.link, nil, nil
	case r.binding != nil:
		return nil, r.binding, nil
	}
	r.binding = make(chan struct{})
	return nil, nil, nil
}

func (r *boxRunner) bindAndPublish(ctx context.Context) (*link, error) {
	r.mu.Lock()
	stale := r.link
	r.link = nil
	r.mu.Unlock()
	if stale != nil {
		stale.close()
	}
	l, err := r.bind(ctx)
	r.mu.Lock()
	defer r.mu.Unlock()
	close(r.binding)
	r.binding = nil
	if err == nil && r.closed {
		l.close()
		return nil, errRunnerClosed
	}
	r.link = l
	return l, err
}

func (r *boxRunner) bind(ctx context.Context) (*link, error) {
	info, err := r.b.threadInfo(r.thread)
	if err != nil {
		return nil, err
	}
	if info.created.ID == "" {
		return nil, fmt.Errorf("remote: thread %s has no session.created", r.thread)
	}
	svc, err := r.b.service(ctx, r.spec)
	if err != nil {
		return nil, err
	}
	cwd := info.created.Cwd
	bound := info.bound
	if bound == nil {
		bound, err = r.parentBinding(info.created)
		if err != nil {
			return nil, err
		}
	}
	if bound == nil {
		return r.create(ctx, svc, cwd, info.created.ForkOf)
	}
	return r.reattach(ctx, svc, cwd, *bound, info.bound)
}

func (r *boxRunner) parentBinding(created event.SessionCreated) (*event.RunnerBound, error) {
	if created.ForkOf == "" {
		return nil, nil
	}
	parent, err := r.b.threadInfo(created.ForkOf)
	return parent.bound, err
}

func (r *boxRunner) create(ctx context.Context, svc sandboxsvc.Service, cwd, parent string) (*link, error) {
	name := "dalgona-" + r.thread
	id, createErr := svc.Create(ctx, sandboxsvc.Spec{Name: name, Image: r.spec.Image, Template: r.spec.Template})
	id = cmp.Or(id, name)
	current, l, err := connect(ctx, svc, id)
	if err != nil && createErr != nil {
		return nil, fmt.Errorf("remote: create box for %s: %w", r.thread, errors.Join(createErr, err))
	}
	if err != nil {
		return nil, errors.Join(err, svc.Destroy(context.WithoutCancel(ctx), id))
	}
	l.cwd = cwd
	bound := event.RunnerBound{Runner: r.name, Service: r.spec.Service, BoxID: current}
	bound.Baseline, err = r.b.upload(ctx, l.remote, cwd)
	if err == nil {
		err = r.b.record(ctx, r.thread, bound)
	}
	if err != nil {
		l.close()
		return nil, errors.Join(err, svc.Destroy(context.WithoutCancel(ctx), current))
	}
	if parent != "" {
		r.b.recordShared(ctx, parent, bound)
	}
	return l, nil
}

func (r *boxRunner) reattach(ctx context.Context, svc sandboxsvc.Service, cwd string, bound event.RunnerBound, journaled *event.RunnerBound) (*link, error) {
	books, err := r.b.readLedger()
	if err != nil {
		return nil, err
	}
	attachID := books.resolve(boxOf(bound)).id
	current, l, err := connect(ctx, svc, attachID)
	if err != nil {
		return nil, err
	}
	l.cwd = cwd
	final, err := r.reconcile(ctx, l, bound, current)
	if err == nil && current != attachID {
		err = r.b.replaced(ctx, r.thread, final, attachID)
	}
	if err == nil && (journaled == nil || *journaled != final) {
		err = r.b.record(ctx, r.thread, final)
	}
	if err != nil {
		l.close()
		return nil, err
	}
	return l, nil
}

func (r *boxRunner) reconcile(ctx context.Context, l *link, bound event.RunnerBound, current string) (event.RunnerBound, error) {
	bound.BoxID = current
	if !losesFiles(bound.Service) {
		return bound, nil
	}
	_, statErr := l.remote.Stat(ctx, baselineFile)
	if statErr == nil {
		return bound, nil
	}
	if !errors.Is(statErr, fs.ErrNotExist) {
		return bound, fmt.Errorf("remote: box %s baseline: %w", current, statErr)
	}
	baseline, err := r.b.upload(ctx, l.remote, l.cwd)
	if err != nil {
		return bound, err
	}
	bound.Baseline = baseline
	message := fmt.Sprintf("box %s was replaced; unpulled box changes are lost", current)
	r.b.log.Warn(message, slog.String("thread", r.thread))
	r.b.note("%s", message)
	return bound, nil
}

func losesFiles(service string) bool { return service == "cloudflare" }

func (r *boxRunner) do(ctx context.Context, fn func(l *link) error) error {
	for attempt := 0; ; attempt++ {
		l, err := r.ensure(ctx)
		if err != nil {
			return err
		}
		err = fn(l)
		if err == nil || attempt > 0 || ctx.Err() != nil || !l.wire.dead.Load() {
			return err
		}
	}
}

func (r *boxRunner) Start(ctx context.Context, spec runner.ProcessSpec) error {
	return r.do(ctx, func(l *link) error {
		boxed := spec
		boxed.Dir = l.boxPath(spec.Dir)
		return l.remote.Start(ctx, boxed)
	})
}

func (r *boxRunner) Read(ctx context.Context, id string, offset int64) (runner.Output, error) {
	var out runner.Output
	err := r.do(ctx, func(l *link) error {
		var err error
		out, err = l.remote.Read(ctx, id, offset)
		return err
	})
	return out, err
}

func (r *boxRunner) Signal(ctx context.Context, id string, s runner.Signal) error {
	return r.do(ctx, func(l *link) error { return l.remote.Signal(ctx, id, s) })
}

func (r *boxRunner) ReadFile(ctx context.Context, p string) ([]byte, error) {
	var data []byte
	err := r.do(ctx, func(l *link) error {
		var err error
		data, err = l.remote.ReadFile(ctx, l.boxPath(p))
		return err
	})
	return data, err
}

func (r *boxRunner) WriteFile(ctx context.Context, p string, data []byte, perm fs.FileMode) error {
	return r.do(ctx, func(l *link) error { return l.remote.WriteFile(ctx, l.boxPath(p), data, perm) })
}

func (r *boxRunner) Stat(ctx context.Context, p string) (runner.FileInfo, error) {
	var info runner.FileInfo
	err := r.do(ctx, func(l *link) error {
		var err error
		info, err = l.remote.Stat(ctx, l.boxPath(p))
		return err
	})
	return info, err
}

func (r *boxRunner) ReadDir(ctx context.Context, p string) ([]runner.FileInfo, error) {
	var infos []runner.FileInfo
	err := r.do(ctx, func(l *link) error {
		var err error
		infos, err = l.remote.ReadDir(ctx, l.boxPath(p))
		return err
	})
	return infos, err
}

func (r *boxRunner) Remove(ctx context.Context, p string) error {
	return r.do(ctx, func(l *link) error { return l.remote.Remove(ctx, l.boxPath(p)) })
}

func (r *boxRunner) Close() error {
	r.mu.Lock()
	r.closed = true
	l := r.link
	r.link = nil
	r.mu.Unlock()
	if l != nil {
		l.close()
	}
	return nil
}

var boxKillGrace = 2 * time.Second

func stopProcess(r runner.Runner, id string) {
	runner.StopProcessWithGrace(r, id, boxKillGrace)
}

func boxRun(ctx context.Context, r runner.Runner, argv ...string) (string, int, error) {
	var random [8]byte
	if _, err := rand.Read(random[:]); err != nil {
		return "", 0, fmt.Errorf("remote: process id: %w", err)
	}
	id := "remote-" + hex.EncodeToString(random[:])
	if err := r.Start(ctx, runner.ProcessSpec{ID: id, Argv: argv, Dir: boxWorkspace}); err != nil {
		stopProcess(r, id)
		return "", 0, fmt.Errorf("remote: start %s: %w", argv[0], err)
	}
	var out bytes.Buffer
	var offset int64
	ticker := time.NewTicker(25 * time.Millisecond)
	defer ticker.Stop()
	for {
		chunk, err := r.Read(ctx, id, offset)
		if err != nil {
			stopProcess(r, id)
			return "", 0, fmt.Errorf("remote: read process %s: %w", id, err)
		}
		out.Write(chunk.Data)
		offset = max(chunk.Next, offset+int64(len(chunk.Data)))
		if chunk.Exit != nil {
			return out.String(), chunk.Exit.Code, nil
		}
		select {
		case <-ctx.Done():
			stopProcess(r, id)
			return "", 0, ctx.Err()
		case <-ticker.C:
		}
	}
}
