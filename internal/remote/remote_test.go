package remote

import (
	"context"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/journal"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
	"github.com/gosuda/dalgona/internal/sandboxsvc/fake"
)

type fakeBox struct {
	svc      *fake.Service
	uploads  int
	logs     *logCapture
	binder   *Binder
	store    *journal.Store
	recorder *journalRecorder
}

func newFakeBinder(t *testing.T, store *journal.Store) (*Binder, *fakeBox) {
	t.Helper()
	runs := t.TempDir()
	box := &fakeBox{logs: &logCapture{}, store: store, recorder: &journalRecorder{store: store, closed: map[string]bool{}}}
	box.svc = fake.New(func(conn net.Conn) {
		local, err := runner.NewLocal(runs)
		if err != nil {
			t.Error(err)
			return
		}
		_ = runner.Serve(context.Background(), conn, local)
	})
	b := NewBinder(Options{
		Config:  config.Config{Home: t.TempDir(), Runners: map[string]config.RunnerSpec{"box": {Service: "fake"}}},
		Journal: store,
		Logger:  slog.New(box.logs),
	})
	b.newSvc = func(context.Context, config.RunnerSpec) (sandboxsvc.Service, error) { return box.svc, nil }
	b.upload = func(context.Context, runner.Runner, string) (string, error) {
		box.uploads++
		return "baseline-1", nil
	}
	b.SetRecorder(box.recorder)
	box.binder = b
	return b, box
}

func (box *fakeBox) use(ctx context.Context, t *testing.T, thread string) {
	t.Helper()
	r := openRunner(ctx, t, box.binder, thread)
	_, err := r.Stat(ctx, os.TempDir())
	must(t, err)
	must(t, r.Close())
}

func (box *fakeBox) archive(ctx context.Context, t *testing.T, thread string, wantDestroyed int) {
	t.Helper()
	must(t, box.binder.Archive(ctx, thread))
	must(t, appendEvents(ctx, box.store, thread, event.ThreadArchived{Archived: true}))
	if got := box.svc.DestroyCount(); got != wantDestroyed {
		t.Fatalf("after archiving %s destroyed %d boxes, want %d", thread, got, wantDestroyed)
	}
}

func TestForkSharesBoxAndArchiveDestroysOnLastThread(t *testing.T) {
	ctx := t.Context()
	store := openJournal(t)
	b, box := newFakeBinder(t, store)
	shared := filepath.Join(t.TempDir(), "f")
	want := event.RunnerBound{Runner: "box", Service: "fake", BoxID: "dalgona-thread-a", Baseline: "baseline-1"}

	startThread(t, store, "thread-a", t.TempDir(), "box")
	a := openRunner(ctx, t, b, "thread-a")
	must(t, a.WriteFile(ctx, shared, []byte("from a"), 0o644))
	requireBindings(t, store, "thread-a", want)

	startFork(t, store, "thread-b", "thread-a")
	fork := openRunner(ctx, t, b, "thread-b")
	must(t, appendEvents(ctx, store, "thread-b", want))
	data, err := fork.ReadFile(ctx, shared)
	if err != nil || string(data) != "from a" {
		t.Fatalf("thread-b read %q, %v", data, err)
	}
	requireBindings(t, store, "thread-b", want)
	if box.svc.CreateCount() != 1 || box.uploads != 1 {
		t.Fatalf("creates %d uploads %d, want 1 and 1", box.svc.CreateCount(), box.uploads)
	}
	must(t, a.Close())
	must(t, fork.Close())

	box.archive(ctx, t, "thread-a", 0)
	if line := "runner: box dalgona-thread-a kept; still used by thread thread-b"; !box.logs.has(line) {
		t.Fatalf("missing log %q in %q", line, box.logs.messages)
	}
	box.archive(ctx, t, "thread-b", 1)
	requireGC(ctx, t, b, 0)
}

func TestForkInheritsParentBoxBoundAfterTheFork(t *testing.T) {
	ctx := t.Context()
	store := openJournal(t)
	_, box := newFakeBinder(t, store)

	startThread(t, store, "thread-p", t.TempDir(), "box")
	startFork(t, store, "thread-f", "thread-p")
	box.use(ctx, t, "thread-p")
	box.use(ctx, t, "thread-f")
	parent := bindings(t, store, "thread-p")
	requireBindings(t, store, "thread-f", parent...)

	startThread(t, store, "thread-q", t.TempDir(), "box")
	startFork(t, store, "thread-g", "thread-q")
	box.use(ctx, t, "thread-g")
	child := bindings(t, store, "thread-g")
	requireBindings(t, store, "thread-q", child...)
	box.use(ctx, t, "thread-q")
	if box.svc.CreateCount() != 2 {
		t.Fatalf("creates %d, want one box per fork family", box.svc.CreateCount())
	}
}

func TestCreateRecoversBoxLeftByCrash(t *testing.T) {
	ctx := t.Context()
	store := openJournal(t)
	b, box := newFakeBinder(t, store)
	_, err := box.svc.Create(ctx, sandboxsvc.Spec{Name: "dalgona-thread-c"})
	must(t, err)
	b.newSvc = func(context.Context, config.RunnerSpec) (sandboxsvc.Service, error) {
		return duplicateRefusing{box.svc}, nil
	}
	startThread(t, store, "thread-c", t.TempDir(), "box")
	box.use(ctx, t, "thread-c")
	requireBindings(t, store, "thread-c", event.RunnerBound{Runner: "box", Service: "fake", BoxID: "dalgona-thread-c", Baseline: "baseline-1"})
	if box.svc.CreateCount() != 1 {
		t.Fatalf("creates %d, want only the crashed one", box.svc.CreateCount())
	}
}

type duplicateRefusing struct{ *fake.Service }

func (d duplicateRefusing) Create(ctx context.Context, spec sandboxsvc.Spec) (string, error) {
	if _, err := d.Attach(ctx, spec.Name); err == nil {
		return "", os.ErrExist
	}
	return d.Service.Create(ctx, spec)
}

func TestReplacementUpdatesEveryThreadOnTheBox(t *testing.T) {
	ctx := t.Context()
	store := openJournal(t)
	b, box := newFakeBinder(t, store)
	_, err := box.svc.Create(ctx, sandboxsvc.Spec{Name: "box-x"})
	must(t, err)
	replacing := &replacingService{Service: box.svc, from: "box-x", to: "box-x-2"}
	b.newSvc = func(context.Context, config.RunnerSpec) (sandboxsvc.Service, error) { return replacing, nil }
	old := event.RunnerBound{Runner: "box", Service: "fake", BoxID: "box-x", Baseline: "base"}
	for _, thread := range []string{"thread-a", "thread-b", "thread-c"} {
		startThread(t, store, thread, t.TempDir(), "box")
		must(t, appendEvents(ctx, store, thread, old))
	}
	box.recorder.closed["thread-c"] = true

	box.use(ctx, t, "thread-a")
	moved := old
	moved.BoxID = "box-x-2"
	requireBindings(t, store, "thread-a", old, moved)
	requireBindings(t, store, "thread-b", old, moved)
	requireBindings(t, store, "thread-c", old)

	delete(box.recorder.closed, "thread-c")
	box.use(ctx, t, "thread-c")
	requireBindings(t, store, "thread-c", old, moved)
	if replacing.replacements != 1 {
		t.Fatalf("old box attached %d times, want 1", replacing.replacements)
	}
}

type replacingService struct {
	*fake.Service
	from, to     string
	replacements int
}

func (s *replacingService) Attach(ctx context.Context, id string) (string, error) {
	if id != s.from {
		return s.Service.Attach(ctx, id)
	}
	s.replacements++
	return s.Create(ctx, sandboxsvc.Spec{Name: s.to})
}

func TestTokenIgnoresProviderAPIKeyEnvironment(t *testing.T) {
	home := t.TempDir()
	store, err := auth.New(home)
	must(t, err)
	must(t, store.SetAPIKey("cloudflare", "worker-secret"))
	t.Setenv("CLOUDFLARE_API_KEY", "account-key")
	t.Setenv("DALGONA_CLOUDFLARE_TOKEN", "")
	b := NewBinder(Options{Config: config.Config{Home: home}, Auth: store})
	if got, err := b.token("cloudflare", "DALGONA_CLOUDFLARE_TOKEN"); err != nil || got != "worker-secret" {
		t.Fatalf("token = %q, %v; want the auth.json key", got, err)
	}
}

func requireGC(ctx context.Context, t *testing.T, b *Binder, wantListed int) []Box {
	t.Helper()
	boxes, err := b.GC(ctx)
	if err != nil || len(boxes) != wantListed {
		t.Fatalf("gc listed %+v, %v; want %d boxes", boxes, err, wantListed)
	}
	return boxes
}

func requireBindings(t *testing.T, store *journal.Store, thread string, want ...event.RunnerBound) {
	t.Helper()
	if got := bindings(t, store, thread); !slices.Equal(got, want) {
		t.Fatalf("%s bindings = %+v, want %+v", thread, got, want)
	}
}

func must(t *testing.T, err error) {
	t.Helper()
	if err != nil {
		t.Fatal(err)
	}
}

func openRunner(ctx context.Context, t *testing.T, b *Binder, thread string) runner.Runner {
	t.Helper()
	r, err := b.Runner(ctx, thread, "box")
	must(t, err)
	return r
}

func TestRunnerRecoversAfterRestart(t *testing.T) {
	ctx := t.Context()
	store := openJournal(t)
	b, box := newFakeBinder(t, store)
	startThread(t, store, "thread-r", t.TempDir(), "box")
	first := openRunner(ctx, t, b, "thread-r")
	spec := runner.ProcessSpec{ID: "call-1", Argv: []string{"sh", "-c", "sleep 0.3; echo done"}, Dir: t.TempDir()}
	must(t, first.Start(ctx, spec))
	must(t, first.Close())

	restarted, _ := newFakeBinder(t, store)
	restarted.newSvc = b.newSvc
	second := openRunner(ctx, t, restarted, "thread-r")
	defer func() { _ = second.Close() }()
	if out := waitOutput(ctx, t, second, "call-1"); out != "done\n" {
		t.Fatalf("output %q", out)
	}
	if box.svc.CreateCount() != 1 || len(bindings(t, store, "thread-r")) != 1 {
		t.Fatalf("restart re-created the box: creates %d", box.svc.CreateCount())
	}
}

func TestGCDestroysConfirmedUnboundBoxes(t *testing.T) {
	ctx := t.Context()
	store := openJournal(t)
	b, box := newFakeBinder(t, store)
	startThread(t, store, "thread-g", t.TempDir(), "box")
	box.use(ctx, t, "thread-g")
	must(t, appendEvents(ctx, store, "thread-g", event.ThreadArchived{Archived: true}))

	requireGC(ctx, t, b, 1)
	if box.svc.DestroyCount() != 0 {
		t.Fatal("unconfirmed gc destroyed a box")
	}
	b.opts.Confirm = func(string) (bool, error) { return true, nil }
	if boxes := requireGC(ctx, t, b, 1); boxes[0].ID != "dalgona-thread-g" || box.svc.DestroyCount() != 1 {
		t.Fatalf("confirmed gc listed %+v, destroyed %d", boxes, box.svc.DestroyCount())
	}
	requireGC(ctx, t, b, 0)
}

func TestBoxPathMapsThreadCwdToWorkspace(t *testing.T) {
	l := &link{cwd: "/home/u/proj"}
	for in, want := range map[string]string{
		"":                         "/workspace",
		"/home/u/proj":             "/workspace",
		"/home/u/proj/":            "/workspace",
		"/home/u/proj/a/b.go":      "/workspace/a/b.go",
		"/home/u/proj/./a":         "/workspace/a",
		"/home/u/proj/../proj/a":   "/workspace/a",
		"/home/u/proj/a\\b":        "/workspace/a\\b",
		"/home/u/proj/../../etc/x": "/home/u/proj/../../etc/x",
		"/home/u/project":          "/home/u/project",
		"/etc/hosts":               "/etc/hosts",
		"/":                        "/",
		"rel/x":                    "rel/x",
	} {
		if got := l.boxPath(in); got != want {
			t.Errorf("boxPath(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestCloudflareRefusesFullyCloudMode(t *testing.T) {
	ctx := t.Context()
	cfg := config.Config{
		Home:    t.TempDir(),
		Remotes: map[string]config.RemoteSpec{"cf": {Service: "cloudflare", URL: "https://worker.example", BoxID: "dalgona-x"}},
		Runners: map[string]config.RunnerSpec{"edge": {Service: "cloudflare", URL: "https://worker.example"}},
	}
	b := NewBinder(Options{Config: cfg})
	const want = "cloudflare boxes lose their files when the container stops; use sandbox0, modal, sprites, or docker for fully cloud mode"
	checks := map[string]error{"cloud up": b.CloudUp(ctx, "edge", false)}
	_, checks["remote"] = b.Bridge(ctx, "cf")
	for name, err := range checks {
		if err == nil || err.Error() != want {
			t.Errorf("%s error = %v, want %q", name, err, want)
		}
	}
}

func waitOutput(ctx context.Context, t *testing.T, r runner.Runner, id string) string {
	t.Helper()
	var out []byte
	var offset int64
	ticker := time.NewTicker(20 * time.Millisecond)
	defer ticker.Stop()
	for {
		chunk, err := r.Read(ctx, id, offset)
		if err != nil {
			t.Fatalf("read %s: %v", id, err)
		}
		out = append(out, chunk.Data...)
		offset = max(chunk.Next, offset+int64(len(chunk.Data)))
		if chunk.Exit != nil {
			return string(out)
		}
		select {
		case <-ctx.Done():
			t.Fatal(ctx.Err())
		case <-ticker.C:
		}
	}
}
