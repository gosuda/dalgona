package remote

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"os/exec"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

type Progress func(phase client.ProgressPhase, completed, total uint64)

type PullResult struct {
	ChangedPaths    []string
	ConflictPaths   []string
	BindingRevision string
}

type GCFailure struct {
	BoxID string
	Kind  string
}

type GCResult struct {
	Candidates      []Box
	Destroyed       []Box
	Failures        []GCFailure
	PreviewRevision string
}

type StalePreviewError struct{ Current string }

func (e *StalePreviewError) Error() string {
	return "remote: the runner ledger changed since the preview; review the new candidates"
}

type Outcome struct {
	ExitCode int
	Output   string
}

func (b *Binder) PullReport(ctx context.Context, threadID string, progress Progress) (PullResult, error) {
	if progress == nil {
		progress = func(client.ProgressPhase, uint64, uint64) {}
	}
	info, err := b.threadInfo(threadID)
	if err != nil {
		return PullResult{}, err
	}
	if info.bound == nil {
		return PullResult{}, fmt.Errorf("remote: thread %s has no box", threadID)
	}
	result := PullResult{BindingRevision: agent.BindingRevision(info.created.Runner, info.bound)}
	progress(client.ProgressConnecting, 0, 1)
	svc, err := b.services.Service(ctx, b.runnerSpec(*info.bound))
	if err != nil {
		return result, err
	}
	_, l, err := connect(ctx, svc, info.bound.BoxID)
	if err != nil {
		return result, err
	}
	defer l.close()
	progress(client.ProgressReading, 0, 1)
	diff, err := client.BoxDiff(ctx, l.remote, info.bound.Baseline)
	if err != nil {
		return result, err
	}
	return result, b.merge(ctx, info.created.Cwd, diff, &result, progress)
}

func (b *Binder) merge(ctx context.Context, cwd string, diff []byte, result *PullResult, progress Progress) error {
	if len(diff) == 0 {
		b.note("nothing to pull")
		progress(client.ProgressFinished, 0, 0)
		return nil
	}
	paths, err := diffPaths(ctx, cwd, diff)
	if err != nil {
		return err
	}
	total := uint64(len(paths))
	progress(client.ProgressMerging, 0, total)
	err = client.ApplyDiff(ctx, cwd, diff)
	if conflict, ok := errors.AsType[*client.ConflictError](err); ok {
		result.ConflictPaths = conflict.Files
	}
	if err != nil && result.ConflictPaths == nil {
		return err
	}
	result.ChangedPaths = paths
	progress(client.ProgressPublishing, total, total)
	progress(client.ProgressFinished, total, total)
	return err
}

func diffPaths(ctx context.Context, cwd string, diff []byte) ([]string, error) {
	numstat := exec.CommandContext(ctx, "git", "apply", "--numstat", "-z")
	numstat.Dir = cwd
	numstat.Stdin = bytes.NewReader(diff)
	out, err := numstat.Output()
	if err != nil {
		return nil, fmt.Errorf("remote: list pulled paths: %w", err)
	}
	return numstatPaths(string(out)), nil
}

func numstatPaths(out string) []string {
	fields := strings.Split(out, "\x00")
	paths := []string{}
	for i := 0; i < len(fields); i++ {
		parts := strings.SplitN(fields[i], "\t", 3)
		switch {
		case len(parts) != 3:
		case parts[2] != "":
			paths = append(paths, parts[2])
		case i+2 < len(fields):
			paths = append(paths, fields[i+2])
			i += 2
		}
	}
	return paths
}

func (b *Binder) GCPreview() (GCResult, error) {
	boxes, revision, err := b.gcState()
	if err == nil && len(boxes) == 0 {
		b.note("no unbound boxes")
	}
	b.noteBoxes(boxes)
	return GCResult{Candidates: boxes, PreviewRevision: revision}, err
}

func (b *Binder) noteBoxes(boxes []Box) {
	for _, box := range boxes {
		b.note("%s %s (runner %s; archived threads %s)", box.Service, box.ID, box.Runner, strings.Join(box.Threads, ", "))
	}
}

func (b *Binder) GCConfirmed(ctx context.Context, previewRevision string) (GCResult, error) {
	boxes, revision, err := b.gcState()
	if err != nil {
		return GCResult{}, err
	}
	if revision != previewRevision {
		return GCResult{Candidates: boxes, PreviewRevision: revision}, &StalePreviewError{Current: revision}
	}
	destroyed, failures, err := b.destroyAll(ctx, boxes)
	return GCResult{Candidates: boxes, Destroyed: destroyed, Failures: failures, PreviewRevision: revision}, err
}

func (b *Binder) gcState() ([]Box, string, error) {
	boxes, err := b.unboundBoxes()
	if err != nil {
		return nil, "", err
	}
	revision, err := b.ledgerRevision(boxes)
	return boxes, revision, err
}

func (b *Binder) destroyAll(ctx context.Context, boxes []Box) ([]Box, []GCFailure, error) {
	var destroyed []Box
	var failures []GCFailure
	var errs []error
	for _, box := range boxes {
		err := b.destroy(ctx, box.Runner, boxKey{box.Service, box.ID})
		if err == nil {
			destroyed = append(destroyed, box)
			continue
		}
		kind := "failed"
		if ctx.Err() != nil {
			kind = "cancelled"
		}
		failures = append(failures, GCFailure{BoxID: box.ID, Kind: kind})
		errs = append(errs, err)
	}
	return destroyed, failures, errors.Join(errs...)
}

func NativePull(ctx context.Context, opts Options, threadID string, progress Progress) (PullResult, Outcome) {
	var output bytes.Buffer
	b := cliBinder(opts, IO{Stdout: &output, Stderr: &output})
	result, err := b.PullReport(ctx, threadID, progress)
	return result, outcome(opts, &output, err)
}

func NativeGC(ctx context.Context, opts Options, confirmed bool, previewRevision string) (GCResult, Outcome, error) {
	var output bytes.Buffer
	b := cliBinder(opts, IO{Stdout: &output, Stderr: &output})
	if !confirmed {
		result, err := b.GCPreview()
		return result, outcome(opts, &output, err), nil
	}
	result, err := b.GCConfirmed(ctx, previewRevision)
	if stale, ok := errors.AsType[*StalePreviewError](err); ok {
		return result, Outcome{}, stale
	}
	return result, outcome(opts, &output, err), nil
}

func outcome(opts Options, output *bytes.Buffer, err error) Outcome {
	code := report(output, err)
	text := output.String()
	if opts.Auth != nil {
		text = opts.Auth.Mask(text)
	}
	return Outcome{ExitCode: code, Output: text}
}
