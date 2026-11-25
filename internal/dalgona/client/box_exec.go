package client

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"os/exec"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/runner"
)

const (
	boxWorkspace = "/workspace"
	baselineFile = "/workspace/.dalgona-baseline"
	diffScript   = `cd /workspace && git add -A && out="$HOME/.dalgona/pull.diff" && mkdir -p "$HOME/.dalgona" && git diff --no-ext-diff --no-color --binary --cached "$1" -- . ':(exclude).dalgona-baseline' > "$out" && printf '\n%s' "$out"`
)

var boxKillGrace = 2 * time.Second

type ConflictError struct{ Files []string }

func (e *ConflictError) Error() string {
	return "remote: pull left conflicts in " + strings.Join(e.Files, ", ")
}

func (e *ConflictError) Report() string {
	return "remote: pull left conflict markers in:\n  " + strings.Join(e.Files, "\n  ") + "\n"
}

func BoxDiff(ctx context.Context, r runner.Runner, baseline string) ([]byte, error) {
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

func ApplyDiff(ctx context.Context, cwd string, diff []byte) error {
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

func stopProcess(r runner.Runner, id string) {
	runner.StopProcessWithGrace(r, id, boxKillGrace)
}

func boxRun(ctx context.Context, r runner.Runner, argv ...string) (string, int, error) {
	var random [8]byte
	if _, err := rand.Read(random[:]); err != nil {
		return "", 0, fmt.Errorf("remote: process id: %w", err)
	}
	id := "remote-" + hex.EncodeToString(random[:])
	if err := r.Start(ctx, runner.ProcessSpec{ID: id, Argv: argv, Dir: boxWorkspace}); err != nil {
		stopProcess(r, id)
		return "", 0, fmt.Errorf("remote: start %s: %w", argv[0], err)
	}
	var out bytes.Buffer
	var offset int64
	ticker := time.NewTicker(25 * time.Millisecond)
	defer ticker.Stop()
	for {
		chunk, err := r.Read(ctx, id, offset)
		if err != nil {
			stopProcess(r, id)
			return "", 0, fmt.Errorf("remote: read process %s: %w", id, err)
		}
		out.Write(chunk.Data)
		offset = max(chunk.Next, offset+int64(len(chunk.Data)))
		if chunk.Exit != nil {
			return out.String(), chunk.Exit.Code, nil
		}
		select {
		case <-ctx.Done():
			stopProcess(r, id)
			return "", 0, ctx.Err()
		case <-ticker.C:
		}
	}
}
