package remote

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc"
)

func (b *Binder) CloudUp(ctx context.Context, name string, copyAuth bool) error {
	spec, err := b.remotes.Spec(name)
	if err != nil {
		return err
	}
	svc, err := b.remotes.Service(ctx, spec)
	if err != nil {
		return err
	}
	recorded, err := b.recordedBox(ctx, svc, name, spec)
	if err != nil {
		return err
	}
	current, l, err := connect(ctx, svc, recorded.BoxID)
	if err != nil {
		return err
	}
	defer l.close()
	if trackErr := b.remotes.Track(name, recorded, current); trackErr != nil {
		return trackErr
	}
	if seedErr := b.seedWorkspace(ctx, newRedialRunner(l, svc, current), current); seedErr != nil {
		return seedErr
	}
	if copyAuth {
		if authErr := b.copyAuth(ctx, l.remote); authErr != nil {
			return authErr
		}
	}
	b.note("remote %s: %s box %s", name, recorded.Service, current)
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
	if err := b.remotes.Record(name, spec); err != nil {
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
	spec, svc, err := b.remotes.Boxed(ctx, name)
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
	return b.remotes.Record(name, spec)
}

func (b *Binder) CloudPull(ctx context.Context, name string) error {
	spec, svc, err := b.remotes.Boxed(ctx, name)
	if err != nil {
		return err
	}
	current, l, err := connect(ctx, svc, spec.BoxID)
	if err != nil {
		return err
	}
	defer l.close()
	if err = b.remotes.Track(name, spec, current); err != nil {
		return err
	}
	baseline, err := l.remote.ReadFile(ctx, baselineFile)
	if err != nil {
		return fmt.Errorf("remote: box %s baseline: %w", current, err)
	}
	return b.pullInto(ctx, l.remote, strings.TrimSpace(string(baseline)), b.opts.Config.Cwd)
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
