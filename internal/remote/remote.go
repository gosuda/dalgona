// Package remote binds threads to sandbox boxes (Mode A) and runs the whole
// daemon in a box (Mode B). See docs/spec/runners.md.
package remote

import (
	"bytes"
	"cmp"
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"maps"
	"os"
	"slices"
	"sync"

	sandbox0sdk "github.com/sandbox0-ai/sdk-go"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/journal"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
	"github.com/gosuda/dalgona/internal/sandboxsvc/cloudflare"
	"github.com/gosuda/dalgona/internal/sandboxsvc/docker"
	"github.com/gosuda/dalgona/internal/sandboxsvc/modal"
	"github.com/gosuda/dalgona/internal/sandboxsvc/sandbox0"
	"github.com/gosuda/dalgona/internal/sandboxsvc/sprites"
)

const (
	boxWorkspace = "/workspace"
	baselineFile = "/workspace/.dalgona-baseline"
)

// Recorder appends runner.bound to an open thread's journal. *agent.Host
// implements it through RecordRunnerBound.
type Recorder interface {
	RecordRunnerBound(ctx context.Context, threadID string, b event.RunnerBound) error
}

// Options configures a Binder. Config and Journal are required for Mode A;
// Mode B needs Config only.
type Options struct {
	Config  config.Config
	Auth    *auth.Store
	Journal *journal.Store
	// Local serves runner names that are not [runners.<name>] entries.
	Local func(ctx context.Context, threadID, runnerName string) (runner.Runner, error)
	// Confirm asks the user a yes or no question. Nil answers no.
	Confirm func(prompt string) (bool, error)
	// Out receives user-facing notes. Nil discards them.
	Out    io.Writer
	Logger *slog.Logger
}

// Binder owns box bindings for one daemon. Its Runner method is the
// agent.HostConfig.Runner factory.
type Binder struct {
	opts     Options
	log      *slog.Logger
	newSvc   func(ctx context.Context, spec config.RunnerSpec) (sandboxsvc.Service, error)
	upload   func(ctx context.Context, r runner.Runner, cwd string) (string, error)
	mu       sync.Mutex
	recorder Recorder
	services map[config.RunnerSpec]sandboxsvc.Service
	remotes  map[string]config.RemoteSpec
	ledgerMu sync.Mutex
}

func NewBinder(opts Options) *Binder {
	logger := cmp.Or(opts.Logger, slog.Default())
	opts.Out = cmp.Or[io.Writer](opts.Out, io.Discard)
	remotes := maps.Clone(opts.Config.Remotes)
	if remotes == nil {
		remotes = map[string]config.RemoteSpec{}
	}
	b := &Binder{
		opts: opts, log: logger, upload: runner.Upload,
		services: map[config.RunnerSpec]sandboxsvc.Service{}, remotes: remotes,
	}
	b.newSvc = b.defaultService
	return b
}

// SetRecorder installs the runner.bound recorder. The host exists only after
// its factory, so the daemon calls this right after agent.NewHost.
func (b *Binder) SetRecorder(r Recorder) {
	b.mu.Lock()
	b.recorder = r
	b.mu.Unlock()
}

// Runner returns a lazily bound box runner for [runners.<name>] entries and
// defers other names to Options.Local. Binding happens on first use, after
// the thread's session.created (and a fork's copied runner.bound) is journaled.
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
	b.mu.Lock()
	defer b.mu.Unlock()
	if svc, ok := b.services[spec]; ok {
		return svc, nil
	}
	svc, err := b.newSvc(ctx, spec)
	if err != nil {
		return nil, err
	}
	b.services[spec] = svc
	return svc, nil
}

func (b *Binder) defaultService(_ context.Context, spec config.RunnerSpec) (sandboxsvc.Service, error) {
	switch spec.Service {
	case "docker":
		return docker.NewFromEnv(runner.Bootstrap)
	case "sprites":
		token, err := b.token("sprites", "SPRITES_TOKEN")
		if err != nil {
			return nil, err
		}
		return sprites.NewFromToken(token, runner.Bootstrap), nil
	case "modal":
		if spec.Image != "" {
			return modal.NewFromEnvWithImage(spec.Image, runner.Bootstrap)
		}
		return modal.NewFromEnv(runner.Bootstrap)
	case "sandbox0":
		return b.sandbox0Service()
	case "cloudflare":
		return b.cloudflareService(spec)
	default:
		return nil, fmt.Errorf("remote: unknown service %q", spec.Service)
	}
}

func (b *Binder) sandbox0Service() (sandboxsvc.Service, error) {
	token, err := b.token("sandbox0", "SANDBOX0_TOKEN")
	if err != nil {
		return nil, err
	}
	opts := []sandbox0sdk.Option{sandbox0sdk.WithToken(token)}
	if base := os.Getenv("SANDBOX0_BASE_URL"); base != "" {
		opts = append(opts, sandbox0sdk.WithBaseURL(base))
	}
	client, err := sandbox0sdk.NewClient(opts...)
	if err != nil {
		return nil, fmt.Errorf("sandbox0: create client: %w", err)
	}
	return sandbox0.New(client, runner.Bootstrap, b.log), nil
}

func (b *Binder) cloudflareService(spec config.RunnerSpec) (sandboxsvc.Service, error) {
	if spec.URL == "" {
		return nil, errors.New("remote: cloudflare runner needs url")
	}
	token, err := b.token("cloudflare", "DALGONA_CLOUDFLARE_TOKEN")
	if err != nil {
		return nil, err
	}
	return cloudflare.New(spec.URL, token), nil
}

func (b *Binder) token(provider, env string) (string, error) {
	if key := os.Getenv(env); key != "" {
		return key, nil
	}
	if key := b.storedKey(provider); key != "" {
		return key, nil
	}
	return "", fmt.Errorf("remote: %s needs %s or `dalgona login %s --api-key`", provider, env, provider)
}

func (b *Binder) storedKey(provider string) string {
	if b.opts.Auth == nil {
		return ""
	}
	for _, entry := range b.opts.Auth.Entries() {
		if entry.ID == provider {
			return entry.Credential.Key
		}
	}
	return ""
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
