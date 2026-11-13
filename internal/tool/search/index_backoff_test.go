package search

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

type failingRunner struct {
	runner.Runner
	signalErr  error
	readFailed *atomic.Bool
}

func (r failingRunner) Read(ctx context.Context, id string, offset int64) (runner.Output, error) {
	if r.readFailed.CompareAndSwap(false, true) {
		return runner.Output{}, errors.New("read refused")
	}
	return r.Runner.Read(ctx, id, offset)
}

func (r failingRunner) Signal(ctx context.Context, id string, signal runner.Signal) error {
	if r.signalErr != nil {
		return r.signalErr
	}
	return r.Runner.Signal(ctx, id, signal)
}

func requireBuildChildExited(t *testing.T, run runner.Runner, id string) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	out, err := run.Read(ctx, id, 0)
	if err != nil {
		t.Fatalf("read build child %s: %v", id, err)
	}
	if out.Alive || out.Exit == nil {
		t.Fatalf("build child %s is still running after runBuild", id)
	}
}

func sleepingBuild(t *testing.T) *buildLog {
	t.Helper()
	return newBuildLog(t, func(run runner.Runner, spec runner.ProcessSpec) error {
		spec.Argv = []string{"sh", "-c", "sleep 30"}
		return run.Start(context.Background(), spec)
	})
}

func TestBuildIsKilledWhenReadFails(t *testing.T) {
	local := newIndexLocal(t)
	log := sleepingBuild(t)
	root := filepath.Join(t.TempDir(), "root")
	buildWG.Add(1)
	runBuild(failingRunner{Runner: local, readFailed: new(atomic.Bool)}, t.TempDir(), root, runner.Sandbox{Mode: runner.SandboxWorkspaceWrite})
	spec := <-log.specs
	requireBuildChildExited(t, local, spec.ID)
	requirePublishOutcome(t, root, 1)
}

func TestBuildKeepsGenerationWhenKillUnconfirmed(t *testing.T) {
	local := newIndexLocal(t)
	log := sleepingBuild(t)
	root := filepath.Join(t.TempDir(), "root")
	buildWG.Add(1)
	runBuild(failingRunner{Runner: local, readFailed: new(atomic.Bool), signalErr: errors.New("signal refused")}, t.TempDir(), root, runner.Sandbox{Mode: runner.SandboxWorkspaceWrite})
	spec := <-log.specs
	t.Cleanup(func() { _ = local.Signal(context.Background(), spec.ID, runner.SignalKill) })
	if currentGeneration(root) != "" {
		t.Fatal("a build whose kill was not confirmed was published")
	}
	entries, err := os.ReadDir(root)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 1 || !entries[0].IsDir() {
		t.Fatalf("root holds %d entries, want the one generation of the unconfirmed build", len(entries))
	}
}

func TestFailedBuildRetriesInNewProcess(t *testing.T) {
	setRebuildThreshold(t, bigFileBytes)
	setBuildBackoff(t, time.Hour)
	text := bigText()
	env, root := buildEnv(t, map[string]string{"big1.txt": text, "big2.txt": text})
	log := newBuildLog(t, func(runner.Runner, runner.ProcessSpec) error { return errors.New("build refused") })
	grepOnce(t, env)
	buildWG.Wait()
	if log.count() != 1 {
		t.Fatalf("build starts = %d, want 1", log.count())
	}
	builds.Delete(root)
	grepAs(t, env, "call-2")
	buildWG.Wait()
	if log.count() != 2 {
		t.Fatalf("build starts in a new process = %d, want 2: a failed build blocked the next process", log.count())
	}
}

func TestUnusableIndexBacksOffNewProcesses(t *testing.T) {
	setRebuildThreshold(t, 1)
	env, root := indexedEnv(t, map[string]string{"a.txt": "needle here\n", "b.txt": "other\n"})
	if err := os.Truncate(filepath.Join(root, currentGeneration(root), trigramFile), 0); err != nil {
		t.Fatal(err)
	}
	log := newBuildLog(t, nil)
	grepOnce(t, env)
	buildWG.Wait()
	builds.Delete(root)
	grepAs(t, env, "call-2")
	buildWG.Wait()
	if log.count() != 0 {
		t.Fatalf("build starts after an unusable index in a new process = %d, want 0", log.count())
	}
}

func grepAs(t *testing.T, env tool.Env, id string) outcome {
	t.Helper()
	result, err := runGrep(t.Context(), env, id, grepArgs)
	if err != nil {
		t.Fatal(err)
	}
	return result
}
