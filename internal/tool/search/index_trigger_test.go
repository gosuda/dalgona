package search

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

const bigFileBytes = 1 << 20

var grepArgs = jsontext.Value(`{"mode":"grep","pattern":"needle","limit":10}`)

func bigText() string {
	return strings.Repeat("a", bigFileBytes-1) + "\n"
}

type remoteLike struct{ runner.Runner }

func setRebuildThreshold(t *testing.T, limit int64) {
	t.Helper()
	previous := indexRebuildBytes
	indexRebuildBytes = limit
	t.Cleanup(func() { indexRebuildBytes = previous })
}

func setBuildBackoff(t *testing.T, window time.Duration) {
	t.Helper()
	previous := indexBuildBackoff
	indexBuildBackoff = window
	t.Cleanup(func() { indexBuildBackoff = previous })
}

func setBuildDeadline(t *testing.T, window time.Duration) {
	t.Helper()
	previous := indexBuildDeadline
	indexBuildDeadline = window
	t.Cleanup(func() { indexBuildDeadline = previous })
}

type buildLog struct{ specs chan runner.ProcessSpec }

func newBuildLog(t *testing.T, start func(runner.Runner, runner.ProcessSpec) error) *buildLog {
	t.Helper()
	log := &buildLog{specs: make(chan runner.ProcessSpec, 16)}
	previous := startBuildProcess
	startBuildProcess = func(run runner.Runner, spec runner.ProcessSpec) error {
		log.specs <- spec
		if start == nil {
			return run.Start(context.Background(), spec)
		}
		return start(run, spec)
	}
	t.Cleanup(func() { startBuildProcess = previous })
	return log
}

func (b *buildLog) count() int { return len(b.specs) }

func grepOnce(t *testing.T, env tool.Env) outcome {
	t.Helper()
	result, err := runGrep(t.Context(), env, "call-1", grepArgs)
	if err != nil {
		t.Fatal(err)
	}
	return result
}

func TestBuildStartsAboveThreshold(t *testing.T) {
	setRebuildThreshold(t, bigFileBytes)
	text := bigText()
	env, root := buildEnv(t, map[string]string{"big1.txt": text, "big2.txt": text, "big3.txt": text})
	log := newBuildLog(t, nil)
	t.Cleanup(buildWG.Wait)
	result := grepOnce(t, env)
	buildWG.Wait()
	if result.Unindexed != 3*bigFileBytes {
		t.Fatalf("unindexed bytes = %d, want %d", result.Unindexed, 3*bigFileBytes)
	}
	if log.count() != 1 {
		t.Fatalf("build starts = %d, want 1", log.count())
	}
	if spec := <-log.specs; spec.Argv[1] != "__search-index" || spec.Argv[2] != root {
		t.Fatalf("build argv = %q, want the index root %q", spec.Argv, root)
	}
	if currentGeneration(root) == "" {
		t.Fatal("the finished build was not published")
	}
}

func TestBuildNotStartedBelowThreshold(t *testing.T) {
	env, _ := buildEnv(t, map[string]string{"small.txt": "needle\n"})
	log := newBuildLog(t, nil)
	grepOnce(t, env)
	buildWG.Wait()
	if log.count() != 0 {
		t.Fatalf("build starts = %d, want 0", log.count())
	}
}

func TestFailedBuildIsNotStartedAgainInProcess(t *testing.T) {
	setRebuildThreshold(t, bigFileBytes)
	setBuildBackoff(t, 0)
	text := bigText()
	env, _ := buildEnv(t, map[string]string{"big1.txt": text, "big2.txt": text})
	log := newBuildLog(t, func(runner.Runner, runner.ProcessSpec) error { return errors.New("build refused") })
	grepOnce(t, env)
	grepOnce(t, env)
	buildWG.Wait()
	if log.count() != 1 {
		t.Fatalf("build starts = %d, want 1", log.count())
	}
}

func TestBuildRunsInSearchSandbox(t *testing.T) {
	setRebuildThreshold(t, bigFileBytes)
	env, root := buildEnv(t, map[string]string{"big.txt": bigText()})
	denied := filepath.Join(t.TempDir(), "denied.sock")
	env.Sandbox = runner.Sandbox{Mode: runner.SandboxWorkspaceWrite, DeniedSockets: []string{denied}}
	log := newBuildLog(t, nil)
	t.Cleanup(buildWG.Wait)
	grepOnce(t, env)
	buildWG.Wait()
	if log.count() != 1 {
		t.Fatalf("build starts = %d, want 1", log.count())
	}
	spec := <-log.specs
	if spec.Sandbox.Mode != runner.SandboxWorkspaceWrite {
		t.Fatalf("build sandbox mode = %q, want %q", spec.Sandbox.Mode, runner.SandboxWorkspaceWrite)
	}
	if want := []string{filepath.Join(root, spec.Argv[3])}; !slices.Equal(spec.Sandbox.WritableRoots, want) {
		t.Fatalf("build writable roots = %q, want %q", spec.Sandbox.WritableRoots, want)
	}
	if !slices.Equal(spec.Sandbox.DeniedSockets, []string{denied}) {
		t.Fatalf("build denied sockets = %q, want %q", spec.Sandbox.DeniedSockets, []string{denied})
	}
	if spec.Dir != env.Cwd {
		t.Fatalf("build directory = %q, want the workspace %q", spec.Dir, env.Cwd)
	}
}

func TestNoBuildForRemoteRunner(t *testing.T) {
	setRebuildThreshold(t, bigFileBytes)
	env, _ := buildEnv(t, map[string]string{"big.txt": bigText()})
	env.Runner = remoteLike{env.Runner}
	log := newBuildLog(t, nil)
	grepOnce(t, env)
	buildWG.Wait()
	if log.count() != 0 {
		t.Fatalf("build starts = %d, want 0", log.count())
	}
}

func TestNoBuildWithoutSandbox(t *testing.T) {
	setRebuildThreshold(t, bigFileBytes)
	env, _ := buildEnv(t, map[string]string{"big.txt": bigText()})
	env.Sandbox = runner.Sandbox{Mode: runner.SandboxOff}
	log := newBuildLog(t, nil)
	grepOnce(t, env)
	buildWG.Wait()
	if log.count() != 0 {
		t.Fatalf("build starts = %d, want 0", log.count())
	}
}

func TestCountedBytesOnlyTriggerBuild(t *testing.T) {
	setRebuildThreshold(t, bigFileBytes)
	env, _ := indexedEnv(t, map[string]string{
		"a.txt":    "needle\n",
		"long.txt": strings.Repeat("y", 2*bigFileBytes) + "\n",
	})
	log := newBuildLog(t, nil)
	grepOnce(t, env)
	buildWG.Wait()
	if log.count() != 0 {
		t.Fatalf("build starts = %d, want 0: a file the index rejected was counted", log.count())
	}
}

func TestSearchReturnsBeforeBuildStarts(t *testing.T) {
	setRebuildThreshold(t, bigFileBytes)
	env, _ := buildEnv(t, map[string]string{"big.txt": bigText()})
	gate := make(chan struct{})
	release := sync.OnceFunc(func() { close(gate) })
	t.Cleanup(buildWG.Wait)
	t.Cleanup(release)
	log := newBuildLog(t, func(run runner.Runner, spec runner.ProcessSpec) error {
		<-gate
		return run.Start(context.Background(), spec)
	})
	done := make(chan error, 1)
	go func() {
		_, err := runGrep(t.Context(), env, "call-1", grepArgs)
		done <- err
	}()
	select {
	case err := <-done:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("the search call waited for the build")
	}
	select {
	case <-log.specs:
	case <-time.After(10 * time.Second):
		t.Fatal("no build started")
	}
	release()
}

func TestRunBuildPublishesOnlyAfterExitZero(t *testing.T) {
	cases := []struct {
		name string
		code int
	}{
		{"exit 1 leaves no generation", 1},
		{"exit 0 publishes the generation", 0},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			newBuildLog(t, func(run runner.Runner, spec runner.ProcessSpec) error {
				spec.Argv = []string{"sh", "-c", "exit " + strconv.Itoa(tc.code)}
				return run.Start(context.Background(), spec)
			})
			root := filepath.Join(t.TempDir(), "root")
			buildWG.Add(1)
			runBuild(newIndexLocal(t), t.TempDir(), root, runner.Sandbox{Mode: runner.SandboxWorkspaceWrite})
			requirePublishOutcome(t, root, tc.code)
		})
	}
}

func requirePublishOutcome(t *testing.T, root string, code int) {
	t.Helper()
	gen := currentGeneration(root)
	if code != 0 && gen != "" {
		t.Fatalf("CURRENT names %q after exit %d", gen, code)
	}
	if code == 0 && gen == "" {
		t.Fatal("CURRENT names no generation after exit 0")
	}
	entries, err := os.ReadDir(root)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range entries {
		if entry.IsDir() && entry.Name() != gen {
			t.Fatalf("generation %q is left after the build", entry.Name())
		}
	}
}

func TestQuarantinedIndexWaitsBeforeNextBuild(t *testing.T) {
	setBuildBackoff(t, time.Hour)
	root := filepath.Join(t.TempDir(), "root")
	if err := os.MkdirAll(root, 0o700); err != nil {
		t.Fatal(err)
	}
	now := time.Now()
	if err := recordIndexFailure(root, now); err != nil {
		t.Fatal(err)
	}
	if !buildBlocked(root, now.Add(time.Minute)) {
		t.Fatal("a recorded failure did not hold back the next build")
	}
	if buildBlocked(root, now.Add(time.Hour+time.Second)) {
		t.Fatal("the backoff did not end after its window")
	}
	env := tool.Env{Runner: newIndexLocal(t), Cwd: t.TempDir()}
	log := newBuildLog(t, nil)
	startBuild(root, env)
	buildWG.Wait()
	if log.count() != 0 {
		t.Fatal("a build started inside the backoff window")
	}
	setBuildBackoff(t, 0)
	startBuild(root, env)
	buildWG.Wait()
	if log.count() != 1 {
		t.Fatalf("build starts after the backoff = %d, want 1", log.count())
	}
}

func TestRetryRecordsQuarantine(t *testing.T) {
	env, root := indexedEnv(t, map[string]string{"a.txt": "needle here\n", "b.txt": "other\n"})
	if err := os.Truncate(filepath.Join(root, currentGeneration(root), trigramFile), 0); err != nil {
		t.Fatal(err)
	}
	grepOnce(t, env)
	if !buildBlocked(root, time.Now()) {
		t.Fatal("the quarantined index did not start a backoff")
	}
}

func TestBuildIsKilledAtDeadline(t *testing.T) {
	setBuildDeadline(t, time.Second)
	local := newIndexLocal(t)
	log := newBuildLog(t, func(run runner.Runner, spec runner.ProcessSpec) error {
		spec.Argv = []string{"sh", "-c", "sleep 30"}
		return run.Start(context.Background(), spec)
	})
	root := filepath.Join(t.TempDir(), "root")
	buildWG.Add(1)
	runBuild(local, t.TempDir(), root, runner.Sandbox{Mode: runner.SandboxWorkspaceWrite})
	spec := <-log.specs
	requireBuildChildExited(t, local, spec.ID)
	requirePublishOutcome(t, root, 1)
	if buildBlocked(root, time.Now()) {
		t.Fatal("a build killed at its deadline wrote a backoff file; the backoff is only for unusable indexes")
	}
}
