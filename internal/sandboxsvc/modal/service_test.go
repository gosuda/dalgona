package modal

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"reflect"
	"slices"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

type fakeProcess struct {
	output *bytes.Reader
	input  bytes.Buffer
	code   int
}

func (p *fakeProcess) Read(data []byte) (int, error)  { return p.output.Read(data) }
func (p *fakeProcess) Write(data []byte) (int, error) { return p.input.Write(data) }
func (p *fakeProcess) Close() error                   { return nil }
func (p *fakeProcess) Wait() (int, error)             { return p.code, nil }

type fakeBox struct {
	commands   [][]string
	pollExit   *int
	terminated int
	output     string
}

func (b *fakeBox) Exec(_ context.Context, argv []string) (Process, error) {
	b.commands = append(b.commands, slices.Clone(argv))
	return &fakeProcess{output: bytes.NewReader([]byte(b.output))}, nil
}

func (b *fakeBox) Poll(context.Context) (*int, error) { return b.pollExit, nil }
func (b *fakeBox) Terminate(context.Context) error {
	b.terminated++
	return nil
}

type fakeAPI struct {
	boxes    map[string]*fakeBox
	requests []CreateRequest
	deleted  []string
}

func newFakeAPI() *fakeAPI {
	return &fakeAPI{boxes: make(map[string]*fakeBox)}
}

func (a *fakeAPI) Create(_ context.Context, request CreateRequest) (Box, string, error) {
	a.requests = append(a.requests, request)
	id := fmt.Sprintf("sandbox-%d", len(a.requests))
	box := &fakeBox{}
	a.boxes[id] = box
	return box, id, nil
}

func (a *fakeAPI) FromID(_ context.Context, id string) (Box, error) {
	box := a.boxes[id]
	if box == nil {
		return nil, errors.New("unknown sandbox")
	}
	return box, nil
}

func (a *fakeAPI) DeleteVolume(_ context.Context, name string) error {
	a.deleted = append(a.deleted, name)
	return nil
}

func TestServiceReplacesFinishedSandboxOnSameVolume(t *testing.T) {
	api := newFakeAPI()
	var bootstrapped []string
	bootstrap := func(ctx context.Context, id string, run runner.RunFunc) error {
		bootstrapped = append(bootstrapped, id)
		_, err := run(ctx, []string{"check", "bootstrap"})
		return err
	}
	service := New(api, bootstrap)
	id, err := service.Create(t.Context(), sandboxsvc.Spec{Name: "dalgona-test", Image: "registry.example/image:tag"})
	requireModal(t, err)
	if id != "sandbox-1/dalgona-test" {
		t.Fatalf("id = %q", id)
	}
	if len(bootstrapped) != 0 {
		t.Fatal("Create ran bootstrap")
	}
	_, err = service.Attach(t.Context(), id)
	requireModal(t, err)
	exit := 0
	api.boxes["sandbox-1"].pollExit = &exit
	current, err := service.Attach(t.Context(), id)
	requireModal(t, err)
	if current != "sandbox-2/dalgona-test" {
		t.Fatalf("replacement id = %q", current)
	}
	wantRequests := []CreateRequest{
		{Name: "dalgona-test", Sequence: 1, Volume: "dalgona-test", Image: "registry.example/image:tag", Timeout: 24 * time.Hour, IdleTimeout: time.Hour},
		{Name: "dalgona-test", Sequence: 1, Volume: "dalgona-test", Image: "registry.example/image:tag", Timeout: 24 * time.Hour, IdleTimeout: time.Hour},
	}
	if !reflect.DeepEqual(api.requests, wantRequests) {
		t.Fatalf("requests = %#v, want %#v", api.requests, wantRequests)
	}
	if !reflect.DeepEqual(bootstrapped, []string{id, current}) {
		t.Fatalf("bootstrapped = %#v", bootstrapped)
	}
	if got := api.boxes["sandbox-2"].commands[0]; !reflect.DeepEqual(got, []string{"check", "bootstrap"}) {
		t.Fatalf("bootstrap argv = %#v", got)
	}
	requireModal(t, service.Destroy(t.Context(), current))
	if api.boxes["sandbox-2"].terminated != 1 {
		t.Fatal("replacement was not terminated")
	}
	if !reflect.DeepEqual(api.deleted, []string{"dalgona-test"}) {
		t.Fatalf("deleted volumes = %#v", api.deleted)
	}
}

func TestServiceConnectUsesInstalledBinary(t *testing.T) {
	api := newFakeAPI()
	service := New(api, nil)
	id, err := service.Create(t.Context(), sandboxsvc.Spec{Name: "dalgona-test"})
	requireModal(t, err)
	tests := []struct {
		name     string
		endpoint sandboxsvc.Endpoint
		argv     []string
	}{
		{name: "exec server", endpoint: sandboxsvc.ExecServer, argv: []string{"sh", "-c", `exec "$HOME/.dalgona/bin/dalgona" exec-server --listen stdio:// --root /workspace`}},
		{name: "bridge", endpoint: sandboxsvc.Bridge, argv: []string{"sh", "-c", `exec "$HOME/.dalgona/bin/dalgona" bridge`}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			stream, err := service.Connect(t.Context(), id, test.endpoint)
			requireModal(t, err)
			requireModal(t, stream.Close())
			commands := api.boxes["sandbox-1"].commands
			if got := commands[len(commands)-1]; !reflect.DeepEqual(got, test.argv) {
				t.Fatalf("argv = %#v, want %#v", got, test.argv)
			}
		})
	}
}

func TestServiceRejectsMalformedID(t *testing.T) {
	service := New(newFakeAPI(), nil)
	if _, err := service.Attach(t.Context(), "missing-volume"); err == nil {
		t.Fatal("Attach accepted an id without a volume")
	}
}

func TestServiceTerminatesReplacementWhenBootstrapFails(t *testing.T) {
	api := newFakeAPI()
	bootstrapErr := errors.New("bootstrap failed")
	service := New(api, func(context.Context, string, runner.RunFunc) error {
		return bootstrapErr
	})
	id, err := service.Create(t.Context(), sandboxsvc.Spec{Name: "dalgona-test"})
	requireModal(t, err)
	exit := 0
	api.boxes["sandbox-1"].pollExit = &exit
	_, err = service.Attach(t.Context(), id)
	if !errors.Is(err, bootstrapErr) {
		t.Fatalf("Attach error = %v, want bootstrap failure", err)
	}
	if api.boxes["sandbox-2"].terminated != 1 {
		t.Fatal("failed replacement is still running")
	}
	if len(api.deleted) != 0 {
		t.Fatal("replacement cleanup deleted the persistent volume")
	}
}

func requireModal(t *testing.T, err error) {
	t.Helper()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
}
