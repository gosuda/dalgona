package client

import (
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/registry"
)

type fakeBridge struct {
	fromClient *io.PipeReader
	toRemote   *io.PipeWriter
	fromRemote *io.PipeReader
	toClient   *io.PipeWriter
}

func newFakeBridge() *fakeBridge {
	fromClient, toRemote := io.Pipe()
	fromRemote, toClient := io.Pipe()
	return &fakeBridge{fromClient: fromClient, toRemote: toRemote, fromRemote: fromRemote, toClient: toClient}
}

func (b *fakeBridge) Read(p []byte) (int, error)  { return b.fromRemote.Read(p) }
func (b *fakeBridge) Write(p []byte) (int, error) { return b.toRemote.Write(p) }
func (b *fakeBridge) Close() error {
	_ = b.toRemote.Close()
	return b.fromRemote.Close()
}

type halfBridge struct{ *fakeBridge }

func (b halfBridge) CloseWrite() error { return b.toRemote.Close() }

func TestRemoteConnPassesClientEOFToTheBox(t *testing.T) {
	bridge := newFakeBridge()
	conn := pipeConn(halfBridge{bridge})
	half, ok := conn.(interface{ CloseWrite() error })
	if !ok {
		t.Fatal("remote conn over a half-closable stream has no CloseWrite")
	}
	remoteSaw := serveBox(bridge)
	if _, err := conn.Write([]byte("request")); err != nil {
		t.Fatal(err)
	}
	if err := half.CloseWrite(); err != nil {
		t.Fatal(err)
	}
	requireBoxRead(t, remoteSaw, "request")
	reply, err := io.ReadAll(conn)
	if err != nil || string(reply) != "reply" {
		t.Fatalf("client read %q, %v; want reply then EOF", reply, err)
	}
	if err := conn.Close(); err != nil {
		t.Fatal(err)
	}
}

type boxRead struct {
	data string
	err  error
}

func serveBox(bridge *fakeBridge) <-chan boxRead {
	remoteSaw := make(chan boxRead, 1)
	go func() {
		data, err := io.ReadAll(bridge.fromClient)
		if err == nil {
			_, err = bridge.toClient.Write([]byte("reply"))
		}
		remoteSaw <- boxRead{data: string(data), err: errors.Join(err, bridge.toClient.Close())}
	}()
	return remoteSaw
}

func requireBoxRead(t *testing.T, remoteSaw <-chan boxRead, want string) {
	t.Helper()
	select {
	case got := <-remoteSaw:
		if got.err != nil || got.data != want {
			t.Fatalf("box read %q, %v; want %q", got.data, got.err, want)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("box never saw EOF after the client closed its write half")
	}
}

func TestRemoteConnWithoutHalfCloseOffersNone(t *testing.T) {
	conn := pipeConn(newFakeBridge())
	defer func() { _ = conn.Close() }()
	if _, ok := conn.(interface{ CloseWrite() error }); ok {
		t.Fatal("remote conn claims CloseWrite over a stream that cannot half-close")
	}
}

func TestRemotesRefuseCloudflareEndpoints(t *testing.T) {
	cfg := config.Config{
		Home:    t.TempDir(),
		Remotes: map[string]config.RemoteSpec{"cf": {Service: "cloudflare", URL: "https://worker.example", BoxID: "dalgona-x"}},
		Runners: map[string]config.RunnerSpec{"edge": {Service: "cloudflare", URL: "https://worker.example"}},
	}
	built := 0
	services := registry.New(registry.Options{Build: func(context.Context, config.RunnerSpec) (sandboxsvc.Service, error) {
		built++
		return nil, errors.New("service factory reached")
	}})
	remotes := NewRemotes(cfg, services)
	session := Remote(remotes, "cf")
	_, connectErr := session.Connect(t.Context())
	_, specErr := remotes.Spec("edge")
	for name, err := range map[string]error{"connect": connectErr, "runner spec": specErr} {
		if err == nil || err.Error() != cloudflareRefusal {
			t.Errorf("%s error = %v, want %q", name, err, cloudflareRefusal)
		}
	}
	if built != 0 {
		t.Fatalf("service factory called %d times, want 0", built)
	}
}

func TestRemotesTrackRecordsAReplacedBox(t *testing.T) {
	home := t.TempDir()
	remotes := NewRemotes(config.Config{Home: home}, registry.New(registry.Options{}))
	spec := config.RemoteSpec{Service: "docker", BoxID: "old", Image: "img"}
	if err := remotes.Track("mote", spec, "old"); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(home, "settings.toml")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("unchanged box wrote settings: %v", err)
	}
	if err := remotes.Track("mote", spec, "new"); err != nil {
		t.Fatal(err)
	}
	got, err := remotes.Spec("mote")
	if err != nil {
		t.Fatal(err)
	}
	if got.BoxID != "new" || got.Image != "img" {
		t.Fatalf("spec = %+v, want box new with image img", got)
	}
	requireRecordedBox(t, home, "mote", "new")
}

func requireRecordedBox(t *testing.T, home, name, box string) {
	t.Helper()
	settings, err := os.ReadFile(filepath.Join(home, "settings.toml"))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(settings), "[remotes."+name+"]") || !strings.Contains(string(settings), "box_id = '"+box+"'") {
		t.Fatalf("settings.toml = %s, want [remotes.%s] with box_id %s", settings, name, box)
	}
}
