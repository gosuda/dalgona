package remote

import (
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

const cloudflareRefusal = "cloudflare boxes lose their files when the container stops; use sandbox0, modal, sprites, or docker for fully cloud mode"

func (b *Binder) CloudUp(ctx context.Context, name string, copyAuth bool) error {
	spec, err := b.remoteSpec(name)
	if err != nil {
		return err
	}
	svc, err := b.service(ctx, remoteRunnerSpec(spec))
	if err != nil {
		return err
	}
	if spec, err = b.recordedBox(ctx, svc, name, spec); err != nil {
		return err
	}
	current, l, err := connect(ctx, svc, spec.BoxID)
	if err != nil {
		return err
	}
	defer l.close()
	if err = b.trackReplacement(name, spec, current); err != nil {
		return err
	}
	if err = b.seedWorkspace(ctx, newRedialRunner(l, svc, current), current); err != nil {
		return err
	}
	if copyAuth {
		if err = b.copyAuth(ctx, l.remote); err != nil {
			return err
		}
	}
	b.note("remote %s: %s box %s", name, spec.Service, current)
	return nil
}

func (b *Binder) recordedBox(ctx context.Context, svc sandboxsvc.Service, name string, spec config.RemoteSpec) (config.RemoteSpec, error) {
	if spec.BoxID != "" {
		return spec, nil
	}
	id, err := svc.Create(ctx, sandboxsvc.Spec{Name: "dalgona-remote-" + name, Image: spec.Image, Template: spec.Template})
	if err != nil {
		return spec, fmt.Errorf("remote: create box for %s: %w", name, err)
	}
	spec.BoxID = id
	if err := b.setRemote(name, spec); err != nil {
		destroyCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), time.Minute)
		defer cancel()
		if destroyErr := svc.Destroy(destroyCtx, id); destroyErr != nil {
			return spec, errors.Join(err, destroyErr)
		}
		return spec, err
	}
	return spec, nil
}

func (b *Binder) seedWorkspace(ctx context.Context, r runner.Runner, boxID string) error {
	_, statErr := r.Stat(ctx, baselineFile)
	if statErr == nil {
		return nil
	}
	if !errors.Is(statErr, fs.ErrNotExist) {
		return fmt.Errorf("remote: box %s baseline: %w", boxID, statErr)
	}
	_, err := b.upload(ctx, r, b.opts.Config.Cwd)
	return err
}

func (b *Binder) CloudDown(ctx context.Context, name string, yes bool) error {
	spec, svc, err := b.boxedRemote(ctx, name)
	if err != nil {
		return err
	}
	if !yes {
		ok, err := b.confirm(fmt.Sprintf("destroy %s box %s and every thread in it?", spec.Service, spec.BoxID))
		if err != nil {
			return err
		}
		if !ok {
			return errors.New("remote: cloud down cancelled")
		}
	}
	if err := svc.Destroy(ctx, spec.BoxID); err != nil {
		return fmt.Errorf("remote: destroy box %s: %w", spec.BoxID, err)
	}
	spec.BoxID = ""
	return b.setRemote(name, spec)
}

func (b *Binder) CloudPull(ctx context.Context, name string) error {
	spec, svc, err := b.boxedRemote(ctx, name)
	if err != nil {
		return err
	}
	current, l, err := connect(ctx, svc, spec.BoxID)
	if err != nil {
		return err
	}
	defer l.close()
	if err = b.trackReplacement(name, spec, current); err != nil {
		return err
	}
	baseline, err := l.remote.ReadFile(ctx, baselineFile)
	if err != nil {
		return fmt.Errorf("remote: box %s baseline: %w", current, err)
	}
	return b.pullInto(ctx, l.remote, strings.TrimSpace(string(baseline)), b.opts.Config.Cwd)
}

func (b *Binder) Bridge(ctx context.Context, name string) (io.ReadWriteCloser, error) {
	spec, svc, err := b.boxedRemote(ctx, name)
	if err != nil {
		return nil, err
	}
	current, err := svc.Attach(ctx, spec.BoxID)
	if err != nil {
		return nil, fmt.Errorf("remote: attach box %s: %w", spec.BoxID, err)
	}
	if err = b.trackReplacement(name, spec, current); err != nil {
		return nil, err
	}
	conn, err := svc.Connect(ctx, current, sandboxsvc.Bridge)
	if err != nil {
		return nil, fmt.Errorf("remote: bridge box %s: %w", current, err)
	}
	return conn, nil
}

func (b *Binder) remoteSpec(name string) (config.RemoteSpec, error) {
	b.mu.Lock()
	spec, ok := b.remotes[name]
	b.mu.Unlock()
	if runnerSpec, isRunner := b.opts.Config.Runners[name]; !ok && isRunner {
		spec, ok = config.RemoteSpec{Service: runnerSpec.Service, Image: runnerSpec.Image, Template: runnerSpec.Template, URL: runnerSpec.URL}, true
	}
	if !ok || spec.Service == "" {
		return spec, fmt.Errorf("remote: no [remotes.%s] or [runners.%s] entry with a service", name, name)
	}
	if losesFiles(spec.Service) {
		return spec, errors.New(cloudflareRefusal)
	}
	return spec, nil
}

func (b *Binder) boxedRemote(ctx context.Context, name string) (config.RemoteSpec, sandboxsvc.Service, error) {
	spec, err := b.remoteSpec(name)
	if err != nil {
		return spec, nil, err
	}
	if spec.BoxID == "" {
		return spec, nil, fmt.Errorf("remote: remote %s has no box; run `dalgona cloud up %s`", name, name)
	}
	svc, err := b.service(ctx, remoteRunnerSpec(spec))
	return spec, svc, err
}

func (b *Binder) trackReplacement(name string, spec config.RemoteSpec, current string) error {
	if current == spec.BoxID {
		return nil
	}
	spec.BoxID = current
	return b.setRemote(name, spec)
}

func (b *Binder) setRemote(name string, spec config.RemoteSpec) error {
	table := map[string]any{"service": spec.Service, "box_id": spec.BoxID}
	for key, value := range map[string]string{"image": spec.Image, "template": spec.Template, "url": spec.URL} {
		if value != "" {
			table[key] = value
		}
	}
	if err := config.WriteSettings(b.opts.Config.Home, config.SettingEdit{Key: "remotes." + name, Value: table}); err != nil {
		return fmt.Errorf("remote: record [remotes.%s]: %w", name, err)
	}
	b.mu.Lock()
	b.remotes[name] = spec
	b.mu.Unlock()
	return nil
}

func (b *Binder) copyAuth(ctx context.Context, r runner.Runner) error {
	data, err := os.ReadFile(filepath.Join(b.opts.Config.Home, "auth.json"))
	if err != nil {
		return fmt.Errorf("remote: read auth.json: %w", err)
	}
	out, code, err := boxRun(ctx, r, "sh", "-c", `mkdir -p "$HOME/.dalgona" && printf '\n%s' "$HOME"`)
	if err != nil {
		return err
	}
	if code != 0 {
		return fmt.Errorf("remote: box home probe exited %d: %s", code, strings.TrimSpace(out))
	}
	home := out[strings.LastIndexByte(out, '\n')+1:]
	if err := r.WriteFile(ctx, home+"/.dalgona/auth.json", data, 0o600); err != nil {
		return fmt.Errorf("remote: copy auth.json: %w", err)
	}
	return nil
}

func remoteRunnerSpec(spec config.RemoteSpec) config.RunnerSpec {
	return config.RunnerSpec{Service: spec.Service, Image: spec.Image, Template: spec.Template, URL: spec.URL}
}
