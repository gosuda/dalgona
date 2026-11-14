package search

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

const (
	searchDir     = "search"
	indexHashSize = 8
)

type indexRef struct {
	root string
	gen  string
}

func indexRoot(home, cwd string) (string, error) {
	resolvedHome := home
	if real, err := filepath.EvalSymlinks(home); err == nil {
		resolvedHome = real
	}
	workspace, err := filepath.EvalSymlinks(cwd)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256([]byte(workspace))
	return filepath.Join(resolvedHome, searchDir, hex.EncodeToString(sum[:indexHashSize])), nil
}

func indexDir(env tool.Env) (string, bool) {
	if runtime.GOOS == "darwin" {
		return "", false
	}
	if !tool.IsLocalRunner(env.Runner) || !ctimeSupported || runner.UnavailableCause() != "" {
		return "", false
	}
	if env.Sandbox.Mode != "" && env.Sandbox.Mode != runner.SandboxWorkspaceWrite {
		return "", false
	}
	return protectedIndexRoot(env)
}

func protectedIndexRoot(env tool.Env) (string, bool) {
	if !filepath.IsAbs(env.Home) {
		return "", false
	}
	root, err := indexRoot(env.Home, env.Cwd)
	if err != nil || insideWritable(root, env) {
		return "", false
	}
	return root, true
}

func insideWritable(root string, env tool.Env) bool {
	writable := []string{env.Cwd, os.TempDir()}
	if cache, err := os.UserCacheDir(); err == nil {
		writable = append(writable, cache)
	}
	writable = append(writable, env.Sandbox.WritableRoots...)
	for _, dir := range writable {
		if !escapesRoot(resolvedDir(dir), root) {
			return true
		}
	}
	return false
}

func resolvedDir(dir string) string {
	if real, err := filepath.EvalSymlinks(dir); err == nil {
		return real
	}
	return dir
}

func indexFor(env tool.Env) indexRef {
	root, ok := indexDir(env)
	if !ok {
		return indexRef{}
	}
	return indexRef{root: root, gen: currentGeneration(root)}
}

func currentGeneration(root string) string {
	data, err := os.ReadFile(filepath.Join(root, currentFile))
	if err != nil {
		return ""
	}
	gen := strings.TrimSpace(string(data))
	if !singlePathElement(gen) {
		return ""
	}
	info, err := os.Stat(filepath.Join(root, gen)) //nolint:gosec // generation name is validated by singlePathElement
	if err != nil || !info.IsDir() {
		return ""
	}
	return gen
}

func forgetGeneration(root, gen string) error {
	path := filepath.Join(root, currentFile)
	data, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	if strings.TrimSpace(string(data)) != gen {
		return nil
	}
	if err = os.Remove(path); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	return nil
}

const backoffFile = "backoff"

var (
	indexRebuildBytes  int64 = 32 << 20
	indexBuildBackoff        = 10 * time.Minute
	indexBuildDeadline       = 10 * time.Minute
	builds             sync.Map
	buildWG            sync.WaitGroup
	startBuildProcess  = func(run runner.Runner, spec runner.ProcessSpec) error {
		return run.Start(context.Background(), spec)
	}
)

func startBuild(root string, env tool.Env) {
	if buildBlocked(root, time.Now()) {
		return
	}
	if _, started := builds.LoadOrStore(root, struct{}{}); started {
		return
	}
	buildWG.Add(1)
	go runBuild(env.Runner, env.Cwd, root, env.Sandbox)
}

func runBuild(run runner.Runner, cwd, root string, sandbox runner.Sandbox) {
	defer buildWG.Done()
	start := time.Now()
	gen, dir, err := createGeneration(root, start)
	if err != nil {
		abandonBuild(root, "", err)
		return
	}
	if err = runBuilder(run, buildJob{cwd: cwd, root: root, gen: gen, dir: dir, start: start, sandbox: sandbox}); err != nil {
		abandonBuild(root, dir, err)
		return
	}
	if err = publishGeneration(root, gen); err != nil {
		abandonBuild(root, dir, err)
		return
	}
	if err = removeOtherGenerations(root, gen); err != nil {
		slog.Default().Warn("search: index build failed", "root", root, "error", err)
	}
	clearBuildBackoff(root)
}

type buildJob struct {
	cwd     string
	root    string
	gen     string
	dir     string
	start   time.Time
	sandbox runner.Sandbox
}

var (
	errBuildDeadline = errors.New("search: index build deadline reached")
	errChildRunning  = errors.New("search: index build child may still run")
)

func runBuilder(run runner.Runner, job buildJob) error {
	id, err := newProcessID("search-index-")
	if err != nil {
		return err
	}
	spec := runner.ProcessSpec{
		ID:   id,
		Argv: []string{"@self", "__search-index", job.root, job.gen, strconv.FormatInt(job.start.UnixNano(), 10)},
		Dir:  job.cwd,
		Sandbox: runner.Sandbox{
			Mode:          runner.SandboxWorkspaceWrite,
			WritableRoots: []string{job.dir},
			DeniedSockets: job.sandbox.DeniedSockets,
		},
	}
	if err = startBuildProcess(run, spec); err != nil {
		return fmt.Errorf("search: start index build: %w", err)
	}
	ctx, cancel := context.WithTimeoutCause(context.Background(), indexBuildDeadline, errBuildDeadline)
	defer cancel()
	if _, err = collect(ctx, run, id); err != nil {
		if errors.Is(context.Cause(ctx), errBuildDeadline) {
			err = errBuildDeadline
		}
		return stopBuilder(run, id, err)
	}
	return nil
}

func stopBuilder(run runner.Runner, id string, cause error) error {
	if _, ok := errors.AsType[exitError](cause); ok {
		return cause
	}
	killCtx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if err := run.Signal(killCtx, id, runner.SignalKill); err != nil {
		if errors.Is(err, runner.ErrProcessNotFound) {
			return cause
		}
		return errors.Join(errChildRunning, cause, fmt.Errorf("search: kill index build: %w", err))
	}
	if err := runner.AwaitExit(context.Background(), run, id, 5*time.Second); err != nil {
		return errors.Join(errChildRunning, cause, fmt.Errorf("search: wait for index build exit: %w", err))
	}
	return cause
}

func abandonBuild(root, dir string, cause error) {
	switch {
	case dir == "":
	case errors.Is(cause, errChildRunning):
		slog.Default().Warn("search: index build child did not exit; its generation is kept", "root", root, "generation", dir)
	default:
		if err := os.RemoveAll(dir); err != nil {
			slog.Default().Warn("search: index build left a generation", "root", root, "error", err)
		}
	}
	slog.Default().Warn("search: index build failed", "root", root, "error", cause)
}

func recordIndexFailure(root string, now time.Time) error {
	return os.WriteFile(filepath.Join(root, backoffFile), []byte(strconv.FormatInt(now.UnixNano(), 10)+"\n"), 0o600)
}

func buildBlocked(root string, now time.Time) bool {
	data, err := os.ReadFile(filepath.Join(root, backoffFile))
	if err != nil {
		return false
	}
	stamp, err := strconv.ParseInt(strings.TrimSpace(string(data)), 10, 64)
	if err != nil {
		return false
	}
	return now.Before(time.Unix(0, stamp).Add(indexBuildBackoff))
}

func clearBuildBackoff(root string) {
	if err := os.Remove(filepath.Join(root, backoffFile)); err != nil && !errors.Is(err, fs.ErrNotExist) {
		slog.Default().Warn("search: could not clear the index backoff", "root", root, "error", err)
	}
}
