package remote

import (
	"bytes"
	"context"
	"fmt"
	"os/exec"
	"strings"

	"github.com/gosuda/dalgona/internal/runner"
)

const diffScript = `cd /workspace && git add -A && out="$HOME/.dalgona/pull.diff" && mkdir -p "$HOME/.dalgona" && git diff --no-ext-diff --no-color --binary --cached "$1" -- . ':(exclude).dalgona-baseline' > "$out" && printf '\n%s' "$out"`

type ConflictError struct{ Files []string }

func (e *ConflictError) Error() string {
	return "remote: pull left conflicts in " + strings.Join(e.Files, ", ")
}

func (b *Binder) Pull(ctx context.Context, threadID string) error {
	info, err := b.threadInfo(threadID)
	if err != nil {
		return err
	}
	if info.bound == nil {
		return fmt.Errorf("remote: thread %s has no box", threadID)
	}
	svc, err := b.service(ctx, b.runnerSpec(*info.bound))
	if err != nil {
		return err
	}
	_, l, err := connect(ctx, svc, info.bound.BoxID)
	if err != nil {
		return err
	}
	defer l.close()
	return b.pullInto(ctx, l.remote, info.bound.Baseline, info.created.Cwd)
}

func (b *Binder) pullInto(ctx context.Context, r runner.Runner, baseline, cwd string) error {
	diff, err := boxDiff(ctx, r, baseline)
	if err != nil {
		return err
	}
	if len(diff) == 0 {
		b.note("nothing to pull")
		return nil
	}
	return applyDiff(ctx, cwd, diff)
}

func boxDiff(ctx context.Context, r runner.Runner, baseline string) ([]byte, error) {
	out, code, err := boxRun(ctx, r, "sh", "-c", diffScript, "_", baseline)
	if err != nil {
		return nil, err
	}
	if code != 0 {
		return nil, fmt.Errorf("remote: box diff exited %d: %s", code, strings.TrimSpace(out))
	}
	diffPath := out[strings.LastIndexByte(out, '\n')+1:]
	diff, err := r.ReadFile(ctx, diffPath)
	if err != nil {
		return nil, fmt.Errorf("remote: read box diff: %w", err)
	}
	return diff, nil
}

func applyDiff(ctx context.Context, cwd string, diff []byte) error {
	prefix, prefixErr := exec.CommandContext(ctx, "git", "-C", cwd, "rev-parse", "--show-prefix").Output()
	inRepo := prefixErr == nil
	args := []string{"-C", cwd, "apply"}
	if inRepo {
		args = append(args, "--3way")
	}
	if dir := strings.TrimSpace(string(prefix)); inRepo && dir != "" {
		args = append(args, "--directory="+dir)
	}
	apply := exec.CommandContext(ctx, "git", args...)
	apply.Stdin = bytes.NewReader(diff)
	output, applyErr := apply.CombinedOutput()
	if applyErr == nil {
		return nil
	}
	if inRepo {
		unmerged, err := exec.CommandContext(ctx, "git", "-C", cwd, "diff", "--name-only", "--relative", "-z", "--diff-filter=U").Output()
		if files := strings.FieldsFunc(string(unmerged), isNUL); err == nil && len(files) > 0 {
			return &ConflictError{Files: files}
		}
	}
	return fmt.Errorf("remote: git apply: %w: %s", applyErr, strings.TrimSpace(string(output)))
}

func isNUL(r rune) bool { return r == 0 }
