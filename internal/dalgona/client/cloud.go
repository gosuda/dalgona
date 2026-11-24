package client

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

type ProgressPhase string

const (
	ProgressConnecting ProgressPhase = "connecting"
	ProgressReading    ProgressPhase = "reading"
	ProgressMerging    ProgressPhase = "merging"
	ProgressPublishing ProgressPhase = "publishing"
	ProgressFinished   ProgressPhase = "finished"
)

type (
	Progress struct {
		Endpoint    EndpointID
		OperationID string
		Phase       ProgressPhase
		Done, Total uint64
	}

	CloudRequest struct {
		Endpoint    EndpointID
		OperationID string
		Progress    func(Progress)
	}

	CloudUpRequest struct {
		CloudRequest
		Workspace string
		CopyAuth  bool
	}

	CloudPullRequest struct {
		CloudRequest
		Workspace string
	}

	CloudDownRequest struct {
		CloudRequest
		Box string
	}

	CloudBox struct{ Service, BoxID string }
)

var ErrBoxChanged = errors.New("remote: the box changed after confirmation; confirm the new box")

type progressSteps struct {
	request CloudRequest
	done    uint64
	total   uint64
}

func (p *progressSteps) enter(phase ProgressPhase) {
	if p.request.Progress != nil {
		p.request.Progress(Progress{Endpoint: p.request.Endpoint, OperationID: p.request.OperationID, Phase: phase, Done: p.done, Total: p.total})
	}
	p.done++
}

func (p *progressSteps) finish() {
	p.done = p.total
	p.enter(ProgressFinished)
}

func (r *Remotes) CloudUp(ctx context.Context, req CloudUpRequest) (CloudBox, error) {
	name := string(req.Endpoint)
	steps := &progressSteps{request: req.CloudRequest, total: 2}
	if req.CopyAuth {
		steps.total++
	}
	steps.enter(ProgressConnecting)
	spec, err := r.boxSpec(name)
	if err != nil {
		return CloudBox{}, err
	}
	svc, err := r.Service(ctx, spec)
	if err != nil {
		return CloudBox{}, err
	}
	if spec, err = r.recordedBox(ctx, svc, name, spec); err != nil {
		return CloudBox{}, err
	}
	current, remote, err := r.openExec(ctx, name, spec, svc)
	if err != nil {
		return CloudBox{}, err
	}
	defer func() { _ = remote.Close() }()
	steps.enter(ProgressPublishing)
	if err := r.seedWorkspace(ctx, r.redialing(remote, name, svc), current, req.Workspace); err != nil {
		return CloudBox{}, err
	}
	if req.CopyAuth {
		steps.enter(ProgressPublishing)
		if err := r.copyAuth(ctx, remote); err != nil {
			return CloudBox{}, err
		}
	}
	steps.finish()
	return CloudBox{Service: spec.Service, BoxID: current}, nil
}

func (r *Remotes) CloudPull(ctx context.Context, req CloudPullRequest) (bool, error) {
	name := string(req.Endpoint)
	steps := &progressSteps{request: req.CloudRequest, total: 3}
	steps.enter(ProgressConnecting)
	spec, svc, err := r.Boxed(ctx, name)
	if err != nil {
		return false, err
	}
	current, remote, err := r.openExec(ctx, name, spec, svc)
	if err != nil {
		return false, err
	}
	defer func() { _ = remote.Close() }()
	steps.enter(ProgressReading)
	baseline, err := remote.ReadFile(ctx, baselineFile)
	if err != nil {
		return false, fmt.Errorf("remote: box %s baseline: %w", current, err)
	}
	diff, err := BoxDiff(ctx, remote, strings.TrimSpace(string(baseline)))
	if err != nil || len(diff) == 0 {
		steps.finish()
		return false, err
	}
	steps.enter(ProgressMerging)
	if err := ApplyDiff(ctx, req.Workspace, diff); err != nil {
		return false, err
	}
	steps.finish()
	return true, nil
}

func (r *Remotes) CloudDown(ctx context.Context, req CloudDownRequest) error {
	name := string(req.Endpoint)
	steps := &progressSteps{request: req.CloudRequest, total: 2}
	steps.enter(ProgressConnecting)
	spec, svc, err := r.Boxed(ctx, name)
	if err != nil {
		return err
	}
	if spec.BoxID != req.Box {
		return ErrBoxChanged
	}
	steps.enter(ProgressPublishing)
	persisted, _, err := config.LoadRemoteSnapshot(r.home, name)
	if err != nil {
		return err
	}
	if persisted != spec {
		return ErrBoxChanged
	}
	if err := svc.Destroy(ctx, spec.BoxID); err != nil {
		return fmt.Errorf("remote: destroy box %s: %w", spec.BoxID, err)
	}
	if err := r.clearDestroyedBox(name, spec); err != nil {
		return err
	}
	steps.finish()
	return nil
}

func (r *Remotes) recordedBox(ctx context.Context, svc sandboxsvc.Service, name string, spec config.RemoteSpec) (config.RemoteSpec, error) {
	if spec.BoxID != "" {
		return spec, nil
	}
	id, err := svc.Create(ctx, sandboxsvc.Spec{Name: "dalgona-remote-" + name, Image: spec.Image, Template: spec.Template})
	if err != nil {
		return spec, fmt.Errorf("remote: create box for %s: %w", name, err)
	}
	spec.BoxID = id
	if err := r.SaveRemote(name, spec); err != nil {
		destroyCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), time.Minute)
		defer cancel()
		return spec, errors.Join(err, svc.Destroy(destroyCtx, id))
	}
	return spec, nil
}

func (r *Remotes) openExec(ctx context.Context, name string, spec config.RemoteSpec, svc sandboxsvc.Service) (string, *runner.Remote, error) {
	current, stream, err := r.attach(ctx, name, spec, svc, sandboxsvc.ExecServer)
	if err != nil {
		return "", nil, err
	}
	remote, err := runner.NewRemote(ctx, current, stream)
	if err != nil {
		_ = stream.Close()
		return "", nil, err
	}
	return current, remote, nil
}

type redialingRunner struct {
	runner.Runner
	redial func(context.Context) (runner.Runner, error)
}

func (r redialingRunner) Redial(ctx context.Context) (runner.Runner, error) { return r.redial(ctx) }

func (r *Remotes) redialing(remote *runner.Remote, name string, svc sandboxsvc.Service) redialingRunner {
	return redialingRunner{Runner: remote, redial: func(ctx context.Context) (runner.Runner, error) {
		spec, err := r.boxSpec(name)
		if err != nil {
			return nil, err
		}
		_, fresh, err := r.openExec(ctx, name, spec, svc)
		return fresh, err
	}}
}

func (r *Remotes) seedWorkspace(ctx context.Context, box runner.Runner, boxID, workspace string) error {
	_, statErr := box.Stat(ctx, baselineFile)
	if statErr == nil {
		return nil
	}
	if !errors.Is(statErr, fs.ErrNotExist) {
		return fmt.Errorf("remote: box %s baseline: %w", boxID, statErr)
	}
	_, err := r.upload(ctx, box, workspace)
	return err
}

func (r *Remotes) copyAuth(ctx context.Context, box runner.Runner) error {
	data, err := os.ReadFile(filepath.Join(r.home, "auth.json"))
	if err != nil {
		return fmt.Errorf("remote: read auth.json: %w", err)
	}
	out, code, err := boxRun(ctx, box, "sh", "-c", `mkdir -p "$HOME/.dalgona" && printf '\n%s' "$HOME"`)
	if err != nil {
		return err
	}
	if code != 0 {
		return fmt.Errorf("remote: box home probe exited %d: %s", code, strings.TrimSpace(out))
	}
	home := out[strings.LastIndexByte(out, '\n')+1:]
	if err := box.WriteFile(ctx, home+"/.dalgona/auth.json", data, 0o600); err != nil {
		return fmt.Errorf("remote: copy auth.json: %w", err)
	}
	return nil
}
