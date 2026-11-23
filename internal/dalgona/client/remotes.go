package client

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"maps"
	"net"
	"sync"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/registry"
)

const cloudflareRefusal = "cloudflare boxes lose their files when the container stops; use sandbox0, modal, sprites, or docker for fully cloud mode"

type Remotes struct {
	home     string
	runners  map[string]config.RunnerSpec
	services *registry.Registry
	mu       sync.Mutex
	remotes  map[string]config.RemoteSpec
}

func NewRemotes(cfg config.Config, services *registry.Registry) *Remotes {
	remotes := maps.Clone(cfg.Remotes)
	if remotes == nil {
		remotes = map[string]config.RemoteSpec{}
	}
	return &Remotes{home: cfg.Home, runners: cfg.Runners, services: services, remotes: remotes}
}

func (r *Remotes) Spec(name string) (config.RemoteSpec, error) {
	r.mu.Lock()
	spec, ok := r.remotes[name]
	r.mu.Unlock()
	if runnerSpec, isRunner := r.runners[name]; !ok && isRunner {
		spec, ok = config.RemoteSpec{Service: runnerSpec.Service, Image: runnerSpec.Image, Template: runnerSpec.Template, URL: runnerSpec.URL}, true
	}
	if !ok || spec.Service == "" {
		return spec, fmt.Errorf("remote: no [remotes.%s] or [runners.%s] entry with a service", name, name)
	}
	if spec.Service == "cloudflare" {
		return spec, errors.New(cloudflareRefusal)
	}
	return spec, nil
}

func (r *Remotes) Service(ctx context.Context, spec config.RemoteSpec) (sandboxsvc.Service, error) {
	return r.services.Service(ctx, config.RunnerSpec{Service: spec.Service, Image: spec.Image, Template: spec.Template, URL: spec.URL})
}

func (r *Remotes) Boxed(ctx context.Context, name string) (config.RemoteSpec, sandboxsvc.Service, error) {
	spec, err := r.Spec(name)
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
	return r.Record(name, spec)
}

func (r *Remotes) Record(name string, spec config.RemoteSpec) error {
	table := map[string]any{"service": spec.Service, "box_id": spec.BoxID}
	for key, value := range map[string]string{"image": spec.Image, "template": spec.Template, "url": spec.URL} {
		if value != "" {
			table[key] = value
		}
	}
	if err := config.WriteSettings(r.home, config.SettingEdit{Key: "remotes." + name, Value: table}); err != nil {
		return fmt.Errorf("remote: record [remotes.%s]: %w", name, err)
	}
	r.mu.Lock()
	r.remotes[name] = spec
	r.mu.Unlock()
	return nil
}

func (r *Remotes) Bridge(ctx context.Context, name string) (net.Conn, error) {
	spec, svc, err := r.Boxed(ctx, name)
	if err != nil {
		return nil, err
	}
	current, err := svc.Attach(ctx, spec.BoxID)
	if err != nil {
		return nil, fmt.Errorf("remote: attach box %s: %w", spec.BoxID, err)
	}
	if err = r.Track(name, spec, current); err != nil {
		return nil, err
	}
	stream, err := svc.Connect(ctx, current, sandboxsvc.Bridge)
	if err != nil {
		return nil, fmt.Errorf("remote: bridge box %s: %w", current, err)
	}
	return pipeConn(stream), nil
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
