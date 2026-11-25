package main

import (
	"bytes"
	"cmp"
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/battery"
	"github.com/gosuda/dalgona/internal/dalgona/remote"
)

const (
	methodRemotePull = "dalgona/remote/pull"
	methodRemoteGC   = "dalgona/remote/gc"
)

type remoteCall struct {
	ThreadID        string `json:"threadId,omitzero"`
	Yes             bool   `json:"yes,omitzero"`
	PreviewRevision string `json:"previewRevision,omitzero"`
	OperationID     string `json:"dalgonaOperationId,omitzero"`
	Cursor          string `json:"cursor,omitzero"`
	Limit           int    `json:"limit,omitzero"`
}

type remoteResult struct {
	ExitCode int    `json:"exitCode"`
	Output   string `json:"output"`
}

type machineSelection struct {
	remote string
	local  bool
}

func (s machineSelection) remoteName(configured string) string {
	if s.local {
		return ""
	}
	return cmp.Or(s.remote, configured)
}

func splitMachineFlags(args []string) (machineSelection, []string, error) {
	var selection machineSelection
	var remoteSet bool
	rest := make([]string, 0, len(args))
	for i := 0; i < len(args); i++ {
		if value, ok := strings.CutPrefix(args[i], "--remote="); ok {
			selection.remote, remoteSet = value, true
			continue
		}
		if args[i] == "--remote" {
			if i+1 == len(args) {
				return machineSelection{}, nil, errors.New("--remote needs a name")
			}
			selection.remote, remoteSet = args[i+1], true
			i++
			continue
		}
		if args[i] == "--local" && len(rest) == 0 {
			selection.local = true
			continue
		}
		rest = append(rest, args[i])
	}
	if selection.local && remoteSet {
		return machineSelection{}, nil, errors.New("--local cannot be combined with --remote")
	}
	return selection, rest, nil
}

func authStore(cfg config.Config) (*auth.Store, error) {
	return auth.NewWithPatterns(cfg.Home, cfg.Secrets.Patterns, auth.WithBuiltinPatterns(cfg.Secrets.Builtin))
}

func runPull(ctx context.Context, args []string, std stdio) int {
	if len(args) != 1 {
		return usage(std, "pull <thread>")
	}
	return callRemote(ctx, std, methodRemotePull, remoteCall{ThreadID: args[0]})
}

func runRunner(ctx context.Context, args []string, std stdio) int {
	if len(args) == 0 || args[0] != "gc" || len(args) > 2 || (len(args) == 2 && args[1] != "--yes") {
		return usage(std, "runner gc [--yes]")
	}
	return callRemote(ctx, std, methodRemoteGC, remoteCall{Yes: len(args) == 2})
}

func callRemote(ctx context.Context, std stdio, method string, params any) int {
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	session, err := openSession(cfg, std)
	if err != nil {
		return fail(std, 1, err)
	}
	defer func() { _ = session.Close() }()
	conn, err := connect(ctx, session)
	if err != nil {
		return fail(std, 1, err)
	}
	client := rpc.Dial(ctx, conn, quietClient{})
	defer func() { _ = client.Close() }()
	var initialized rpc.InitializeResult
	if err := client.Initialize(ctx, rpc.InitializeParams{ClientInfo: rpc.ClientInfo{Name: binary, Version: config.Version()}}, &initialized); err != nil {
		return fail(std, 1, err)
	}
	var result remoteResult
	if err := client.Call(ctx, method, params, &result); err != nil {
		return fail(std, 1, err)
	}
	fmt.Fprint(std.out, result.Output)
	return result.ExitCode
}

type quietClient struct{}

func (quietClient) Notification(string, jsontext.Value) {}

func (quietClient) Request(context.Context, string, jsontext.Value) (any, error) {
	return nil, errors.New("this command answers no server requests")
}

func remoteExtensions(d *battery.Daemon) map[string]rpcserver.Extension {
	return map[string]rpcserver.Extension{
		methodRemotePull:   remoteExtension(d, remotePull),
		methodRemoteGC:     remoteExtension(d, remoteGC),
		methodRemoteStatus: rpcserver.WithDeadline(rpcserver.MetadataDeadline, remoteExtension(d, nativeStatus)),
	}
}

func remoteExtension(d *battery.Daemon, run func(context.Context, remote.Options, remoteCall) (jsontext.Value, error)) rpcserver.Extension {
	return func(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
		call, err := decodeRemoteCall(params)
		if err != nil {
			return nil, err
		}
		return run(ctx, d.Remote, call)
	}
}

func remotePull(ctx context.Context, opts remote.Options, call remoteCall) (jsontext.Value, error) {
	if rpcserver.NativeFeature(ctx, "runnerManagement") == 1 {
		return nativePull(ctx, opts, call)
	}
	return runCaptured(func(out remote.IO) int { return remote.PullMain(ctx, opts, []string{call.ThreadID}, out) })
}

func remoteGC(ctx context.Context, opts remote.Options, call remoteCall) (jsontext.Value, error) {
	if rpcserver.NativeFeature(ctx, "runnerManagement") == 1 {
		return nativeGC(ctx, opts, call)
	}
	opts.Confirm = func(string) (bool, error) { return call.Yes, nil }
	return runCaptured(func(out remote.IO) int { return remote.RunnerMain(ctx, opts, []string{"gc"}, out) })
}

func decodeRemoteCall(params jsontext.Value) (remoteCall, error) {
	var call remoteCall
	if len(params) == 0 {
		return call, nil
	}
	if err := json.Unmarshal(params, &call); err != nil {
		return remoteCall{}, &rpc.RPCError{Code: -32602, Message: "invalid params: " + err.Error()}
	}
	return call, nil
}

func runCaptured(run func(remote.IO) int) (jsontext.Value, error) {
	var output bytes.Buffer
	code := run(remote.IO{Stdin: strings.NewReader(""), Stdout: &output, Stderr: &output})
	return encodeResult(code, output.String())
}

func encodeResult(code int, output string) (jsontext.Value, error) {
	var encoded bytes.Buffer
	encoder := jsontext.NewEncoder(&encoded)
	tokens := []jsontext.Token{
		jsontext.BeginObject, jsontext.String("exitCode"), jsontext.Int(int64(code)),
		jsontext.String("output"), jsontext.String(output), jsontext.EndObject,
	}
	for _, token := range tokens {
		if err := encoder.WriteToken(token); err != nil {
			return nil, fmt.Errorf("encode command result: %w", err)
		}
	}
	return jsontext.Value(bytes.TrimSpace(encoded.Bytes())), nil
}
