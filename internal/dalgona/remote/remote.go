package remote

import (
	"bytes"
	"cmp"
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"slices"
	"sync"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/journal"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/registry"
)

const (
	boxWorkspace = "/workspace"
	baselineFile = "/workspace/.dalgona-baseline"
)

type Recorder interface {
	RecordRunnerBound(ctx context.Context, threadID string, b event.RunnerBound) error
}

type Options struct {
	Config   config.Config
	Auth     *auth.Store
	Journal  *journal.Store
	Local    func(ctx context.Context, threadID, runnerName string) (runner.Runner, error)
	Confirm  func(prompt string) (bool, error)
	Out      io.Writer
	Logger   *slog.Logger
	Services *registry.Registry
}

type Binder struct {
	opts     Options
	log      *slog.Logger
	services *registry.Registry
	upload   func(ctx context.Context, r runner.Runner, cwd string) (string, error)
	mu       sync.Mutex
	recorder Recorder
	remotes  *client.Remotes
	ledgerMu sync.Mutex
}

func NewBinder(opts Options) *Binder {
	logger := cmp.Or(opts.Logger, slog.Default())
	opts.Out = cmp.Or[io.Writer](opts.Out, io.Discard)
	services := opts.Services
	if services == nil {
		services = registry.New(registry.Options{Auth: opts.Auth, Logger: logger})
	}
	return &Binder{opts: opts, log: logger, upload: runner.Upload, services: services, remotes: client.NewRemotes(opts.Config, services)}
}

func (b *Binder) SetRecorder(r Recorder) {
	b.mu.Lock()
	b.recorder = r
	b.mu.Unlock()
}

func (b *Binder) Runner(ctx context.Context, threadID, runnerName string) (runner.Runner, error) {
	spec, ok := b.opts.Config.Runners[runnerName]
	if !ok {
		if b.opts.Local == nil {
			return nil, fmt.Errorf("remote: runner %s is not a [runners.<name>] entry", runnerName)
		}
		return b.opts.Local(ctx, threadID, runnerName)
	}
	if spec.Service == "" {
		return nil, fmt.Errorf("remote: runners.%s has no service", runnerName)
	}
	return &boxRunner{b: b, thread: threadID, name: runnerName, spec: spec}, nil
}

func (b *Binder) record(ctx context.Context, threadID string, bound event.RunnerBound) error {
	b.mu.Lock()
	recorder := b.recorder
	b.mu.Unlock()
	if recorder == nil {
		return errors.New("remote: no runner.bound recorder; call SetRecorder")
	}
	if err := recorder.RecordRunnerBound(ctx, threadID, bound); err != nil {
		return fmt.Errorf("remote: record runner.bound for %s: %w", threadID, err)
	}
	return nil
}

func (b *Binder) note(format string, args ...any) {
	_, _ = fmt.Fprintf(b.opts.Out, format+"\n", args...)
}

func (b *Binder) confirm(prompt string) (bool, error) {
	if b.opts.Confirm == nil {
		return false, nil
	}
	return b.opts.Confirm(prompt)
}

func (b *Binder) service(ctx context.Context, spec config.RunnerSpec) (sandboxsvc.Service, error) {
	return b.services.Service(ctx, spec)
}

func (b *Binder) runnerSpec(bound event.RunnerBound) config.RunnerSpec {
	if spec, ok := b.opts.Config.Runners[bound.Runner]; ok && spec.Service == bound.Service {
		return spec
	}
	return config.RunnerSpec{Service: bound.Service}
}

type threadInfo struct {
	created  event.SessionCreated
	bound    *event.RunnerBound
	archived bool
}

var threadTags = [][]byte{
	[]byte(`"` + event.TypeSessionCreated + `"`),
	[]byte(`"` + event.TypeRunnerBound + `"`),
	[]byte(`"` + event.TypeThreadArchived + `"`),
}

func (b *Binder) threadInfo(threadID string) (threadInfo, error) {
	var info threadInfo
	if b.opts.Journal == nil {
		return info, errors.New("remote: thread operations need the journal")
	}
	err := b.opts.Journal.Replay(threadID, func(seq uint64, raw []byte) error {
		if !slices.ContainsFunc(threadTags, func(tag []byte) bool { return bytes.Contains(raw, tag) }) {
			return nil
		}
		decoded, err := event.Decode(raw)
		if err != nil {
			return fmt.Errorf("seq %d: %w", seq, err)
		}
		info.apply(decoded)
		return nil
	})
	damage, damaged := errors.AsType[*journal.DamageError](err)
	switch {
	case damaged:
		b.log.Warn("remote: thread journal is damaged; using the records before the damage",
			slog.String("thread", threadID), slog.Uint64("seq", damage.Seq))
	case err != nil:
		return info, fmt.Errorf("remote: read thread %s: %w", threadID, err)
	}
	return info, nil
}

func (info *threadInfo) apply(decoded event.Event) {
	switch value := decoded.(type) {
	case event.SessionCreated:
		info.created = value
	case event.RunnerBound:
		info.bound = &value
	case event.ThreadArchived:
		info.archived = value.Archived
	}
}

type redialRunner struct {
	runner.Runner
	connect func(ctx context.Context) (runner.Runner, error)
}

func (r *redialRunner) Redial(ctx context.Context) (runner.Runner, error) {
	return r.connect(ctx)
}

func newRedialRunner(l *link, svc sandboxsvc.Service, boxID string) *redialRunner {
	return &redialRunner{
		Runner: l.remote,
		connect: func(ctx context.Context) (runner.Runner, error) {
			_, fresh, err := connect(ctx, svc, boxID)
			if err != nil {
				return nil, err
			}
			return fresh.remote, nil
		},
	}
}

func connect(ctx context.Context, svc sandboxsvc.Service, boxID string) (string, *link, error) {
	current, err := svc.Attach(ctx, boxID)
	if err != nil {
		return "", nil, fmt.Errorf("remote: attach box %s: %w", boxID, err)
	}
	conn, err := svc.Connect(ctx, current, sandboxsvc.ExecServer)
	if err != nil {
		return "", nil, fmt.Errorf("remote: connect box %s: %w", current, err)
	}
	w := &wire{ReadWriteCloser: conn}
	r, err := runner.NewRemote(ctx, current, w)
	if err != nil {
		_ = conn.Close()
		return "", nil, err
	}
	return current, &link{remote: r, wire: w}, nil
}
