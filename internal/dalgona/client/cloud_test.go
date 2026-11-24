package client

import (
	"context"
	"errors"
	"net"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/fake"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/registry"
)

type cloudFixture struct {
	home     string
	svc      *fake.Service
	remotes  *Remotes
	uploads  int
	built    int
	mu       sync.Mutex
	progress []Progress
}

func newCloudFixture(t *testing.T, specs map[string]config.RemoteSpec) *cloudFixture {
	t.Helper()
	runs := t.TempDir()
	f := &cloudFixture{home: t.TempDir()}
	f.svc = fake.New(func(conn net.Conn) {
		local, err := runner.NewLocal(runs)
		if err != nil {
			t.Error(err)
			return
		}
		_ = runner.Serve(context.Background(), conn, local)
	})
	cfg := config.Config{Home: f.home, Remotes: specs, Runners: map[string]config.RunnerSpec{"edge": {Service: "cloudflare", URL: "https://worker.example"}}}
	f.remotes = NewRemotes(cfg, registry.New(registry.Options{Build: func(context.Context, config.RunnerSpec) (sandboxsvc.Service, error) {
		f.built++
		return f.svc, nil
	}}))
	f.remotes.upload = func(context.Context, runner.Runner, string) (string, error) {
		f.uploads++
		return "baseline-1", nil
	}
	return f
}

func (f *cloudFixture) request(endpoint EndpointID) CloudRequest {
	return CloudRequest{Endpoint: endpoint, OperationID: "op-1", Progress: func(p Progress) {
		f.mu.Lock()
		f.progress = append(f.progress, p)
		f.mu.Unlock()
	}}
}

func (f *cloudFixture) requireProgress(t *testing.T, want ...Progress) {
	t.Helper()
	f.mu.Lock()
	defer f.mu.Unlock()
	if !slices.Equal(f.progress, want) {
		t.Fatalf("progress = %+v, want %+v", f.progress, want)
	}
}

func (f *cloudFixture) recorded(t *testing.T, name string) config.RemoteSpec {
	t.Helper()
	t.Setenv("DALGONA_HOME", f.home)
	cfg, err := config.Load(config.ProductDalgona, t.TempDir(), config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	return cfg.Remotes[name]
}

func step(phase ProgressPhase, done, total uint64) Progress {
	return Progress{Endpoint: "work", OperationID: "op-1", Phase: phase, Done: done, Total: total}
}

func TestCloudUpCreatesRecordsAndSeedsTheBox(t *testing.T) {
	f := newCloudFixture(t, map[string]config.RemoteSpec{"work": {Service: "fake"}})
	box, err := f.remotes.CloudUp(t.Context(), CloudUpRequest{CloudRequest: f.request("work"), Workspace: t.TempDir()})
	if err != nil {
		t.Fatal(err)
	}
	if box != (CloudBox{Service: "fake", BoxID: "dalgona-remote-work"}) || f.svc.CreateCount() != 1 || f.uploads != 1 {
		t.Fatalf("box %+v after %d creates and %d uploads, want dalgona-remote-work, 1 and 1", box, f.svc.CreateCount(), f.uploads)
	}
	if got := f.recorded(t, "work"); got != (config.RemoteSpec{Service: "fake", BoxID: "dalgona-remote-work"}) {
		t.Fatalf("recorded remote %+v", got)
	}
	f.requireProgress(t, step(ProgressConnecting, 0, 2), step(ProgressPublishing, 1, 2), step(ProgressFinished, 2, 2))
}

func TestCloudDownDestroysOnlyTheConfirmedBox(t *testing.T) {
	f := newCloudFixture(t, nil)
	id, err := f.svc.Create(t.Context(), sandboxsvc.Spec{Name: "box-1"})
	if err != nil {
		t.Fatal(err)
	}
	if err := f.remotes.SaveRemote("work", config.RemoteSpec{Service: "fake", BoxID: id}); err != nil {
		t.Fatal(err)
	}
	stale := CloudDownRequest{CloudRequest: CloudRequest{Endpoint: "work"}, Box: "box-0"}
	if err := f.remotes.CloudDown(t.Context(), stale); !errors.Is(err, ErrBoxChanged) || f.svc.DestroyCount() != 0 {
		t.Fatalf("down with a stale confirmation = %v after %d destroys, want ErrBoxChanged and none", err, f.svc.DestroyCount())
	}
	if err := f.remotes.CloudDown(t.Context(), CloudDownRequest{CloudRequest: f.request("work"), Box: id}); err != nil {
		t.Fatal(err)
	}
	if f.svc.DestroyCount() != 1 || f.recorded(t, "work").BoxID != "" {
		t.Fatalf("destroyed %d boxes, recorded %+v; want one destroy and no box", f.svc.DestroyCount(), f.recorded(t, "work"))
	}
	f.requireProgress(t, step(ProgressConnecting, 0, 2), step(ProgressPublishing, 1, 2), step(ProgressFinished, 2, 2))
}

func TestCloudDownRefusesPersistedReplacementBeforeDestroy(t *testing.T) {
	f, replacement, req := confirmedCloudDown(t)
	if err := replacement.SaveRemote("work", config.RemoteSpec{Service: "fake", BoxID: "box-new"}); err != nil {
		t.Fatal(err)
	}
	if err := f.remotes.CloudDown(t.Context(), req); !errors.Is(err, ErrBoxChanged) || f.svc.DestroyCount() != 0 {
		t.Fatalf("down after replacement: error %v, destroyed %d boxes; want ErrBoxChanged and no destruction", err, f.svc.DestroyCount())
	}
}

func TestCloudDownKeepsReplacementRecordedDuringDestroy(t *testing.T) {
	f, replacement, req := confirmedCloudDown(t)
	f.remotes.services = registry.New(registry.Options{Build: func(context.Context, config.RunnerSpec) (sandboxsvc.Service, error) {
		return replacingService{Service: f.svc, replace: func() error {
			return replacement.SaveRemote("work", config.RemoteSpec{Service: "fake", BoxID: "box-new"})
		}}, nil
	}})
	if err := f.remotes.CloudDown(t.Context(), req); err != nil {
		t.Fatal(err)
	}
	if got := f.recorded(t, "work").BoxID; got != "box-new" || f.svc.DestroyCount() != 1 {
		t.Fatalf("recorded box %q, destroyed %d boxes; want replacement box-new and one destruction", got, f.svc.DestroyCount())
	}
}

type replacingService struct {
	sandboxsvc.Service
	replace func() error
}

func (s replacingService) Destroy(ctx context.Context, id string) error {
	if err := s.replace(); err != nil {
		return err
	}
	return s.Service.Destroy(ctx, id)
}

func confirmedCloudDown(t *testing.T) (*cloudFixture, *Remotes, CloudDownRequest) {
	t.Helper()
	f := newCloudFixture(t, nil)
	id, err := f.svc.Create(t.Context(), sandboxsvc.Spec{Name: "box-old"})
	if err != nil {
		t.Fatal(err)
	}
	spec := config.RemoteSpec{Service: "fake", BoxID: id}
	if err := f.remotes.SaveRemote("work", spec); err != nil {
		t.Fatal(err)
	}
	other := NewRemotes(config.Config{Home: f.home, Remotes: map[string]config.RemoteSpec{"work": spec}}, nil)
	return f, other, CloudDownRequest{CloudRequest: f.request("work"), Box: id}
}

func TestCloudPullReadsTheBaselineOfTheRecordedBox(t *testing.T) {
	f := newCloudFixture(t, nil)
	id, err := f.svc.Create(t.Context(), sandboxsvc.Spec{Name: "box-1"})
	if err != nil {
		t.Fatal(err)
	}
	if saveErr := f.remotes.SaveRemote("work", config.RemoteSpec{Service: "fake", BoxID: id}); saveErr != nil {
		t.Fatal(saveErr)
	}
	_, err = f.remotes.CloudPull(t.Context(), CloudPullRequest{CloudRequest: f.request("work"), Workspace: t.TempDir()})
	if err == nil || !strings.HasPrefix(err.Error(), "remote: box box-1 baseline: ") {
		t.Fatalf("pull without a baseline = %v, want the baseline error", err)
	}
	f.requireProgress(t, step(ProgressConnecting, 0, 3), step(ProgressReading, 1, 3))
}

func TestCloudOperationsRefuseDaemonAndCloudflareRemotes(t *testing.T) {
	f := newCloudFixture(t, map[string]config.RemoteSpec{"work": {Kind: config.RemoteDaemon, URL: "https://work.example", TokenFile: "/unused"}})
	daemon := "remote: work is a daemon remote and has no box; connect to it from the desktop client"
	for name, err := range map[string]error{
		"up":   second(f.remotes.CloudUp(t.Context(), CloudUpRequest{CloudRequest: CloudRequest{Endpoint: "work"}})),
		"down": f.remotes.CloudDown(t.Context(), CloudDownRequest{CloudRequest: CloudRequest{Endpoint: "work"}}),
		"pull": second(f.remotes.CloudPull(t.Context(), CloudPullRequest{CloudRequest: CloudRequest{Endpoint: "work"}})),
	} {
		if err == nil || err.Error() != daemon {
			t.Fatalf("cloud %s on a daemon remote = %v, want %q", name, err, daemon)
		}
	}
	_, err := f.remotes.CloudUp(t.Context(), CloudUpRequest{CloudRequest: CloudRequest{Endpoint: "edge"}})
	if err == nil || err.Error() != cloudflareRefusal || f.built != 0 {
		t.Fatalf("cloud up on cloudflare = %v after %d service builds, want the refusal and none", err, f.built)
	}
}

func second[T any](_ T, err error) error { return err }

func TestRecordedBoxDestroysTheBoxWhenTheRecordFails(t *testing.T) {
	home, err := os.Create(filepath.Join(t.TempDir(), "home-file"))
	if err != nil {
		t.Fatal(err)
	}
	if err := home.Close(); err != nil {
		t.Fatal(err)
	}
	r := NewRemotes(config.Config{Home: home.Name()}, nil)
	svc := fake.New()
	if _, err := r.recordedBox(t.Context(), svc, "mote", config.RemoteSpec{Service: "docker"}); err == nil {
		t.Fatal("recordedBox error = nil, want settings write error")
	}
	if got := svc.DestroyCount(); got != 1 {
		t.Fatalf("destroyed %d boxes, want 1 orphan cleanup", got)
	}
}

func managedSet(t *testing.T, home string) *EndpointSet {
	t.Helper()
	cfg := config.Config{Home: home}
	remotes := NewRemotes(cfg, nil)
	set, err := NewEndpointSet(remotes, ConfiguredEndpoints(cfg, Backend{}, remotes), newRecorder(), ProtocolNative)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = set.Close() })
	return set
}

func TestSaveRemoteRefusesInvalidFieldsAndNamesBeforeWriting(t *testing.T) {
	home := t.TempDir()
	set := managedSet(t, home)
	for name, test := range map[string]struct {
		spec config.RemoteSpec
		want string
	}{
		"work":     {config.RemoteSpec{Kind: config.RemoteDaemon, URL: "http://work.example", TokenFile: "work.token"}, "remotes.work.url must be an https:// or wss:// URL with a host and no user info, query, or fragment"},
		"work.box": {config.RemoteSpec{Service: "docker"}, `remote: name "work.box" must use only letters, digits, - and _`},
	} {
		if err := set.SaveRemote(name, test.spec); err == nil || err.Error() != test.want {
			t.Fatalf("saving %s = %v, want %q", name, err, test.want)
		}
	}
	if _, err := os.Stat(filepath.Join(home, "settings.toml")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("refused saves wrote settings.toml: %v", err)
	}
	if got := set.Endpoints(); len(got) != 1 {
		t.Fatalf("refused saves changed the endpoints: %+v", got)
	}
}

func TestSaveRemoteAddsThenEditsAnEndpoint(t *testing.T) {
	home := t.TempDir()
	set := managedSet(t, home)
	if err := os.WriteFile(filepath.Join(home, "work.token"), []byte("relay-token\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	daemon := config.RemoteSpec{Kind: config.RemoteDaemon, URL: "https://work.example", TokenFile: "work.token"}
	if err := set.SaveRemote("work", daemon); err != nil {
		t.Fatal(err)
	}
	want := []EndpointStatus{
		{ID: EndpointID(home), Label: "local", Kind: EndpointLocal, Phase: PhaseIdle},
		{ID: "work", Label: "work", Kind: EndpointDaemon, Phase: PhaseIdle},
	}
	if got := set.Endpoints(); !slices.Equal(got, want) {
		t.Fatalf("endpoints = %+v, want %+v", got, want)
	}
	daemon.TokenFile = filepath.Join(home, "work.token")
	if got, err := set.remotes.Spec("work"); err != nil || got != daemon {
		t.Fatalf("read daemon configuration for edit: got %+v, %v; want %+v", got, err, daemon)
	}
	f := &cloudFixture{home: home}
	if got := f.recorded(t, "work"); got != daemon {
		t.Fatalf("recorded %+v, want %+v", got, daemon)
	}
	if err := set.SaveRemote("work", config.RemoteSpec{Service: "docker"}); err != nil || set.Endpoints()[1].Kind != EndpointBox {
		t.Fatalf("editing work into a box remote = %v, endpoints %+v", err, set.Endpoints())
	}
}
