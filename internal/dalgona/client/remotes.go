package client

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"maps"
	"net"
	"path/filepath"
	"regexp"
	"sync"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/registry"
)

const (
	cloudflareRefusal = "cloudflare boxes lose their files when the container stops; use sandbox0, modal, sprites, or docker for fully cloud mode"

	RemoteBox    = config.RemoteBox
	RemoteDaemon = config.RemoteDaemon
)

type RemoteSpec = config.RemoteSpec

var remoteNamePattern = regexp.MustCompile(`^[A-Za-z0-9_-]+$`)

type Remotes struct {
	home      string
	tlsCAFile string
	runners   map[string]config.RunnerSpec
	services  *registry.Registry
	upload    func(ctx context.Context, r runner.Runner, cwd string) (string, error)
	mu        sync.Mutex
	remotes   map[string]config.RemoteSpec
}

func NewRemotes(cfg config.Config, services *registry.Registry) *Remotes {
	remotes := maps.Clone(cfg.Remotes)
	if remotes == nil {
		remotes = map[string]config.RemoteSpec{}
	}
	return &Remotes{home: cfg.Home, tlsCAFile: cfg.TLSCAFile, runners: cfg.Runners, services: services, upload: runner.Upload, remotes: remotes}
}

func (r *Remotes) Spec(name string) (config.RemoteSpec, error) {
	spec, ok := r.configuredRemote(name)
	if !ok {
		return spec, fmt.Errorf("remote: no [remotes.%s] entry", name)
	}
	return spec, nil
}

func (r *Remotes) configuredRemote(name string) (config.RemoteSpec, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	spec, ok := r.remotes[name]
	return spec, ok
}

func (r *Remotes) boxSpec(name string) (config.RemoteSpec, error) {
	spec, ok := r.configuredRemote(name)
	if runnerSpec, isRunner := r.runners[name]; !ok && isRunner {
		spec = config.RemoteSpec{Service: runnerSpec.Service, Image: runnerSpec.Image, Template: runnerSpec.Template, URL: runnerSpec.URL}
	}
	return checkBoxRemote(name, spec)
}

func checkBoxRemote(name string, spec config.RemoteSpec) (config.RemoteSpec, error) {
	switch {
	case spec.Kind == config.RemoteDaemon:
		return spec, fmt.Errorf("remote: %s is a daemon remote and has no box; connect to it from the desktop client", name)
	case spec.Kind == config.RemoteSSH || spec.Kind == config.RemoteDevice:
		return spec, fmt.Errorf("remote: %s connects directly and has no box", name)
	case spec.Service == "":
		return spec, fmt.Errorf("remote: no [remotes.%s] or [runners.%s] entry with a service", name, name)
	case spec.Service == "cloudflare":
		return spec, errors.New(cloudflareRefusal)
	}
	return spec, nil
}

func (r *Remotes) Service(ctx context.Context, spec config.RemoteSpec) (sandboxsvc.Service, error) {
	return r.services.Service(ctx, config.RunnerSpec{Service: spec.Service, Image: spec.Image, Template: spec.Template, URL: spec.URL})
}

func (r *Remotes) Boxed(ctx context.Context, name string) (config.RemoteSpec, sandboxsvc.Service, error) {
	spec, err := r.boxSpec(name)
	if err != nil {
		return spec, nil, err
	}
	if spec.BoxID == "" {
		return spec, nil, fmt.Errorf("remote: remote %s has no box; run `dalgona cloud up %s`", name, name)
	}
	svc, err := r.Service(ctx, spec)
	return spec, svc, err
}

func (r *Remotes) Track(name string, spec config.RemoteSpec, current string) error {
	if current == spec.BoxID {
		return nil
	}
	spec.BoxID = current
	return r.SaveRemote(name, spec)
}

func (r *Remotes) SaveRemote(name string, spec config.RemoteSpec) error {
	if !remoteNamePattern.MatchString(name) {
		return fmt.Errorf("remote: name %q must use only letters, digits, - and _", name)
	}
	if spec.TokenFile != "" && !filepath.IsAbs(spec.TokenFile) {
		spec.TokenFile = filepath.Join(r.home, spec.TokenFile)
	}
	if err := config.CheckRemote(name, spec); err != nil {
		return err
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if err := config.WriteSettings(r.home, config.SettingEdit{Key: "remotes." + name, Value: remoteTable(spec)}); err != nil {
		return fmt.Errorf("remote: record [remotes.%s]: %w", name, err)
	}
	r.remotes[name] = spec
	return nil
}

func (r *Remotes) clearDestroyedBox(name string, destroyed config.RemoteSpec) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	current, revision, err := config.LoadRemoteSnapshot(r.home, name)
	if err != nil || current != destroyed {
		return err
	}
	edit := config.SettingEdit{Key: "remotes." + name + ".box_id", Value: ""}
	if _, err := config.WriteSettingsIf(r.home, revision, edit); err != nil {
		return fmt.Errorf("remote: clear destroyed box %s: %w", destroyed.BoxID, err)
	}
	current.BoxID = ""
	r.remotes[name] = current
	return nil
}

func remoteTable(spec config.RemoteSpec) map[string]any {
	switch spec.Kind {
	case config.RemoteSSH:
		return map[string]any{"kind": spec.Kind, "machine_id": spec.MachineID, "ssh_destination": spec.SSHDestination, "ssh_platform": spec.SSHPlatform, "ssh_install": spec.SSHInstall}
	case config.RemoteDevice:
		return map[string]any{"kind": spec.Kind, "machine_id": spec.MachineID, "public_key_pin": spec.PublicKeyPin, "url": spec.URL, "token_file": spec.TokenFile}
	case config.RemoteDaemon:
		return map[string]any{"kind": spec.Kind, "url": spec.URL, "token_file": spec.TokenFile}
	}
	table := map[string]any{"service": spec.Service, "box_id": spec.BoxID}
	for key, value := range map[string]string{"kind": spec.Kind, "image": spec.Image, "template": spec.Template, "url": spec.URL} {
		if value != "" {
			table[key] = value
		}
	}
	return table
}

func (r *Remotes) dial(ctx context.Context, name string) (io.ReadWriteCloser, error) {
	spec, _ := r.configuredRemote(name)
	switch spec.Kind {
	case config.RemoteSSH:
		stream, _, err := dialSSH(ctx, spec)
		if err != nil {
			return nil, err
		}
		return stream, nil
	case config.RemoteDevice:
		return r.dialDevice(ctx, spec)
	case config.RemoteDaemon:
		return r.Relay(ctx, name)
	}
	return r.Bridge(ctx, name)
}

func (r *Remotes) Bridge(ctx context.Context, name string) (net.Conn, error) {
	spec, svc, err := r.Boxed(ctx, name)
	if err != nil {
		return nil, err
	}
	_, stream, err := r.attach(ctx, name, spec, svc, sandboxsvc.Bridge)
	if err != nil {
		return nil, err
	}
	return pipeConn(stream), nil
}

func (r *Remotes) attach(ctx context.Context, name string, spec config.RemoteSpec, svc sandboxsvc.Service, endpoint sandboxsvc.Endpoint) (string, io.ReadWriteCloser, error) {
	current, err := svc.Attach(ctx, spec.BoxID)
	if err != nil {
		return "", nil, fmt.Errorf("remote: attach box %s: %w", spec.BoxID, err)
	}
	if err = r.Track(name, spec, current); err != nil {
		return "", nil, err
	}
	stream, err := svc.Connect(ctx, current, endpoint)
	if err != nil {
		return "", nil, fmt.Errorf("remote: connect box %s: %w", current, err)
	}
	return current, stream, nil
}

type streamConn struct {
	net.Conn
	stream io.ReadWriteCloser
}

func (c streamConn) Write(p []byte) (int, error) { return c.stream.Write(p) }

func (c streamConn) Close() error { return errors.Join(c.Conn.Close(), c.stream.Close()) }

type halfStreamConn struct {
	streamConn
	half interface{ CloseWrite() error }
}

func (c halfStreamConn) CloseWrite() error { return c.half.CloseWrite() }

func pipeConn(stream io.ReadWriteCloser) net.Conn {
	local, far := net.Pipe()
	go func() {
		_, err := io.Copy(far, stream)
		if err != nil && !errors.Is(err, net.ErrClosed) && !errors.Is(err, io.ErrClosedPipe) {
			slog.Debug("remote bridge ended", "error", err)
		}
		_ = far.Close()
	}()
	conn := streamConn{Conn: local, stream: stream}
	if half, ok := stream.(interface{ CloseWrite() error }); ok {
		return halfStreamConn{streamConn: conn, half: half}
	}
	return conn
}
