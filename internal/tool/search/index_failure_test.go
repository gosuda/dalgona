package search

import (
	"context"
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
)

type exitingRunner struct {
	runner.Runner
	output string
	starts int
}

func (r *exitingRunner) Start(context.Context, runner.ProcessSpec) error {
	r.starts++
	return nil
}

func (r *exitingRunner) Read(context.Context, string, int64) (runner.Output, error) {
	return runner.Output{Data: []byte(r.output), Next: int64(len(r.output)), Exit: &runner.Exit{Code: 1}}, nil
}

func (r *exitingRunner) Signal(context.Context, string, runner.Signal) error { return nil }

func TestSandboxFailureIsNotAnIndexFailure(t *testing.T) {
	sandbox := exitError{code: 1, output: []byte("dalgona: sandbox: landlock: permission denied\n")}
	if isIndexFailure(sandbox) {
		t.Fatal("a sandbox failure with status 1 was taken for an unusable index")
	}
	for _, output := range []string{"search: index unusable: trigram size", "corrupt index"} {
		if !isIndexFailure(exitError{code: indexFailedStatus, output: []byte(output)}) {
			t.Fatalf("output %q with status 1 is not an index failure", output)
		}
	}
}

func TestSandboxFailureKeepsCurrent(t *testing.T) {
	env, root := indexedEnv(t, map[string]string{"a.txt": "needle here\n"})
	gen := currentGeneration(root)
	if gen == "" {
		t.Fatal("no generation was published")
	}
	fake := &exitingRunner{Runner: env.Runner, output: "dalgona: sandbox: landlock: permission denied\n"}
	env.Runner = fake
	raw := jsontext.Value(`{"mode":"grep","pattern":"needle","limit":100}`)
	if _, err := grepWithIndex(t.Context(), env, indexRef{root: root, gen: gen}, "call-1", raw); err == nil {
		t.Fatal("a sandbox failure returned no error")
	}
	if fake.starts != 1 {
		t.Fatalf("the call started %d processes, want 1: a sandbox failure must not rerun the call", fake.starts)
	}
	if currentGeneration(root) != gen {
		t.Fatal("a sandbox failure removed CURRENT")
	}
}
