package search

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

type helperRunner struct {
	runner.Runner
	startErr error
	readErr  error
	output   runner.Output
}

func (r *helperRunner) Start(context.Context, runner.ProcessSpec) error { return r.startErr }

func (r *helperRunner) Read(ctx context.Context, _ string, _ int64) (runner.Output, error) {
	if err := ctx.Err(); err != nil {
		return runner.Output{}, err
	}
	if err := r.readErr; err != nil {
		r.readErr = nil
		return runner.Output{}, err
	}
	return r.output, nil
}

func (r *helperRunner) Signal(context.Context, string, runner.Signal) error { return nil }

type helperCase struct {
	name      string
	run       *helperRunner
	args      string
	cancelled bool
	want      tool.FailureKind
}

const findArgs = `{"mode":"find","pattern":"*.txt"}`

func (c helperCase) check(t *testing.T) {
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	if c.cancelled {
		cancel()
	}
	env := tool.Env{Runner: c.run, Cwd: t.TempDir(), Snapshots: snapshot.New()}
	result, err := New().Run(ctx, tool.Call{ID: "search-failure", Args: jsontext.Value(c.args)}, env)
	if err == nil {
		t.Fatal("the failing search returned no error")
	}
	if result.Failure != c.want {
		t.Fatalf("failure = %q, want %q (error: %v)", result.Failure, c.want, err)
	}
}

func exitWith(code int, output string) runner.Output {
	return runner.Output{Data: []byte(output), Next: int64(len(output)), Exit: &runner.Exit{Code: code}}
}

func TestSearchHelperFailureIsInternal(t *testing.T) {
	tests := []helperCase{
		{name: "start", run: &helperRunner{startErr: errors.New("remote: connection closed")}},
		{name: "read", run: &helperRunner{readErr: errors.New("process log unreadable"), output: exitWith(-1, "")}},
		{name: "undecodable outcome", run: &helperRunner{output: exitWith(0, "not an outcome")}},
	}
	for _, test := range tests {
		test.args, test.want = findArgs, tool.FailureInternal
		t.Run(test.name, test.check)
	}
}

func TestSearchOrdinaryFailureStaysOrdinary(t *testing.T) {
	tests := []helperCase{
		{name: "refusal", run: &helperRunner{output: exitWith(refusalStatus, "search: path does not exist")}, args: findArgs},
		{name: "sandbox exit", run: &helperRunner{output: exitWith(1, "dalgona: sandbox: landlock: permission denied\n")}, args: findArgs},
		{name: "cancelled", run: &helperRunner{output: exitWith(-1, "")}, args: findArgs, cancelled: true},
		{name: "invalid arguments", run: &helperRunner{}, args: `{"mode":"nope"}`},
	}
	for _, test := range tests {
		test.want = tool.FailureNone
		t.Run(test.name, test.check)
	}
}
