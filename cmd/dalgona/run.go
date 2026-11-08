package main

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"os"
	"strings"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/app/battery"
	"github.com/gosuda/dalgona/internal/rpc"
	"github.com/gosuda/dalgona/internal/script"
	"github.com/gosuda/dalgona/internal/scripthost"
)

const (
	methodScriptRun = "dalgona/script/run"
	runUsage        = "run <file.star> [--arg k=v ...]"
)

type scriptRun struct {
	Entry  string         `json:"entry"`
	Source string         `json:"source"`
	Args   jsontext.Value `json:"args,omitzero"`
	Cwd    string         `json:"cwd"`
}

func runWorkflow(ctx context.Context, args []string, std stdio) int {
	if len(args) == 0 || strings.HasPrefix(args[0], "-") {
		return usage(std, runUsage)
	}
	values, err := workflowArgs(args[1:])
	if err != nil {
		fmt.Fprintf(std.errOut, "%s: %v\n", binary, err)
		return usage(std, runUsage)
	}
	source, err := os.ReadFile(args[0])
	if err != nil {
		return fail(std, 1, fmt.Errorf("read workflow: %w", err))
	}
	cwd, err := os.Getwd()
	if err != nil {
		return fail(std, 1, fmt.Errorf("working directory: %w", err))
	}
	return callRemote(ctx, std, methodScriptRun, scriptRun{Entry: args[0], Source: string(source), Args: values, Cwd: cwd})
}

func workflowArgs(args []string) (jsontext.Value, error) {
	if len(args) == 0 {
		return nil, nil
	}
	var out bytes.Buffer
	encoder := jsontext.NewEncoder(&out)
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return nil, fmt.Errorf("encode workflow args: %w", err)
	}
	for i := 0; i < len(args); i += 2 {
		if args[i] != "--arg" || i+1 == len(args) {
			return nil, fmt.Errorf("unexpected argument %q", args[i])
		}
		if err := writeArg(encoder, args[i+1]); err != nil {
			return nil, err
		}
	}
	if err := encoder.WriteToken(jsontext.EndObject); err != nil {
		return nil, fmt.Errorf("encode workflow args: %w", err)
	}
	return jsontext.Value(bytes.TrimSpace(out.Bytes())), nil
}

func writeArg(encoder *jsontext.Encoder, pair string) error {
	key, value, ok := strings.Cut(pair, "=")
	if !ok || key == "" {
		return fmt.Errorf("--arg %q is not k=v", pair)
	}
	if err := encoder.WriteToken(jsontext.String(key)); err != nil {
		return fmt.Errorf("encode workflow arg %s: %w", key, err)
	}
	raw := jsontext.Value(value)
	if raw.IsValid() {
		return encoder.WriteValue(raw)
	}
	return encoder.WriteToken(jsontext.String(value))
}

func scriptExtension(host *agent.Host, d *battery.Daemon) rpc.Extension {
	return func(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
		var run scriptRun
		if err := json.Unmarshal(params, &run); err != nil || run.Entry == "" {
			return nil, &rpc.RPCError{Code: -32602, Message: "invalid params: " + methodScriptRun + " needs entry and source"}
		}
		return runOnThread(ctx, host, d.Scripts, run)
	}
}

func runOnThread(ctx context.Context, host *agent.Host, scripts scripthost.Config, run scriptRun) (_ jsontext.Value, err error) {
	thread, err := host.Start(ctx, agent.ThreadSpec{Cwd: run.Cwd})
	if err != nil {
		return nil, err
	}
	defer func() { err = errors.Join(err, thread.Close()) }()
	workflow := script.Workflow{ID: thread.ID(), Entry: run.Entry, Source: []byte(run.Source), Args: run.Args}
	outcome, err := scripthost.RunWorkflow(ctx, thread, scripts, workflow)
	if err != nil {
		return nil, err
	}
	return encodeResult(outcomeText(outcome))
}

func outcomeText(outcome script.Outcome) (int, string) {
	var out strings.Builder
	for _, line := range outcome.Prints.Lines {
		out.WriteString(line)
		out.WriteByte('\n')
	}
	if outcome.Status != script.StatusCompleted {
		fmt.Fprintf(&out, "workflow %s: %s (%s at %s)\n", outcome.Status, outcome.Error.Message, outcome.Error.Code, outcome.Error.Stage)
		return 1, out.String()
	}
	out.Write(outcome.Value)
	out.WriteByte('\n')
	return 0, out.String()
}
