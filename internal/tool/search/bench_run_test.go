package search

import (
	"context"
	"encoding/json/jsontext"
	"fmt"
	"math/rand/v2"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

type countingRunner struct {
	runner.Runner
	reads     atomic.Int64
	readNanos atomic.Int64
}

func (r *countingRunner) Read(ctx context.Context, id string, offset int64) (runner.Output, error) {
	r.reads.Add(1)
	start := time.Now()
	out, err := r.Runner.Read(ctx, id, offset)
	r.readNanos.Add(int64(time.Since(start)))
	return out, err
}

func benchToolEnv(b *testing.B, root string) (tool.Env, *countingRunner) {
	b.Helper()
	local, err := runner.NewLocal(filepath.Join(b.TempDir(), "runs"))
	if err != nil {
		b.Fatal(err)
	}
	counted := &countingRunner{Runner: local}
	env := tool.Env{
		Runner: counted, Cwd: root, Sandbox: runner.Sandbox{Mode: runner.SandboxOff},
		Snapshots: snapshot.New(),
	}
	return env, counted
}

func benchToolCall(b *testing.B, args jsontext.Value, counted *countingRunner, env tool.Env) {
	b.Helper()
	if _, err := New().Run(b.Context(), tool.Call{Args: args}, env); err != nil {
		b.Fatal(err)
	}
	before := counted.reads.Load()
	readBefore := counted.readNanos.Load()
	var total time.Duration
	benchSamples(b, nil, func() {
		start := time.Now()
		if _, err := New().Run(b.Context(), tool.Call{Args: args}, env); err != nil {
			b.Fatal(err)
		}
		total += time.Since(start)
	})
	b.ReportMetric(float64(counted.reads.Load()-before)/float64(b.N), "reads/op")
	if total > 0 {
		b.ReportMetric(float64(counted.readNanos.Load()-readBefore)/float64(total.Nanoseconds()), "read_share")
	}
}

func BenchmarkSearchToolLargeOutput(b *testing.B) {
	root := b.TempDir()
	writeTextFiles(b, root, []string{"large.txt"}, 16<<20, rand.New(rand.NewPCG(1, 2)))
	env, counted := benchToolEnv(b, root)
	args := jsontext.Value(`{"mode":"grep","pattern":"line","limit":200}`)
	benchToolCall(b, args, counted, env)
}

func BenchmarkSearchToolSmallFind(b *testing.B) {
	root := b.TempDir()
	names := make([]string, 1500)
	for i := range names {
		names[i] = fmt.Sprintf("file-%05d.txt", i)
	}
	writeTextFiles(b, root, names, 10000, rand.New(rand.NewPCG(1, 2)))
	env, counted := benchToolEnv(b, root)
	args := jsontext.Value(`{"mode":"find","pattern":"file-00042.txt"}`)
	benchToolCall(b, args, counted, env)
}
