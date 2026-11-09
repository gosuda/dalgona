package main

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"strings"

	"github.com/gosuda/dalgona/internal/app/battery"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/remote"
	"github.com/gosuda/dalgona/internal/rpc"
)

const (
	methodRemotePull = "dalgona/remote/pull"
	methodRemoteGC   = "dalgona/remote/gc"
)

type remoteCall struct {
	ThreadID string `json:"threadId,omitzero"`
	Yes      bool   `json:"yes,omitzero"`
}

type remoteResult struct {
	ExitCode int    `json:"exitCode"`
	Output   string `json:"output"`
}

func splitRemoteFlag(args []string) (string, []string, error) {
	rest := make([]string, 0, len(args))
	name := ""
	for i := 0; i < len(args); i++ {
		value, inline := strings.CutPrefix(args[i], "--remote=")
		switch {
		case inline:
			name = value
		case args[i] == "--remote" && i+1 < len(args):
			name = args[i+1]
			i++
		case args[i] == "--remote":
			return "", nil, errors.New("--remote needs a name")
		default:
			rest = append(rest, args[i])
		}
	}
	return name, rest, nil
}

func remoteOptions(cfg config.Config, std stdio) (remote.Options, error) {
	store, err := auth.NewWithPatterns(cfg.Home, cfg.Secrets.Patterns)
	if err != nil {
		return remote.Options{}, err
	}
	return remote.Options{Config: cfg, Auth: store, Out: std.errOut, Logger: slog.New(slog.NewTextHandler(std.errOut, nil))}, nil
}

func remoteDialer(cfg config.Config, name string, std stdio) (func(context.Context) (net.Conn, error), error) {
	opts, err := remoteOptions(cfg, std)
	if err != nil {
		return nil, err
	}
	binder := remote.NewBinder(opts)
	return func(ctx context.Context) (net.Conn, error) {
		stream, err := binder.Bridge(ctx, name)
		if err != nil {
			return nil, err
		}
		return pipeConn(stream), nil
	}, nil
}

type streamConn struct {
	net.Conn
	stream io.ReadWriteCloser
}

func (c streamConn) Write(p []byte) (int, error) { return c.stream.Write(p) }

func (c streamConn) Close() error { return errors.Join(c.Conn.Close(), c.stream.Close()) }

type halfStreamConn struct {
	streamConn
	half interface{ CloseWrite() error }
}

func (c halfStreamConn) CloseWrite() error { return c.half.CloseWrite() }

func pipeConn(stream io.ReadWriteCloser) net.Conn {
	local, far := net.Pipe()
	go func() {
		_, err := io.Copy(far, stream)
		if err != nil && !errors.Is(err, net.ErrClosed) && !errors.Is(err, io.ErrClosedPipe) {
			slog.Debug("remote bridge ended", "error", err)
		}
		_ = far.Close()
	}()
	conn := streamConn{Conn: local, stream: stream}
	if half, ok := stream.(interface{ CloseWrite() error }); ok {
		return halfStreamConn{streamConn: conn, half: half}
	}
	return conn
}

func runCloud(ctx context.Context, args []string, std stdio) int {
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	opts, err := remoteOptions(cfg, std)
	if err != nil {
		return fail(std, 1, err)
	}
	opts.Out = nil
	return remote.CloudMain(ctx, opts, args, remote.IO{Stdin: std.in, Stdout: std.out, Stderr: std.errOut})
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
	dial, err := daemonDialer(cfg, std)
	if err != nil {
		return fail(std, 1, err)
	}
	conn, err := dial(ctx)
	if err != nil {
		return fail(std, 1, err)
	}
	client, err := rpc.Dial(conn, quietClient{})
	if err != nil {
		return fail(std, 1, errors.Join(err, conn.Close()))
	}
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

func remoteExtensions(d *battery.Daemon) map[string]rpc.Extension {
	return map[string]rpc.Extension{
		methodRemotePull: func(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
			call, err := decodeRemoteCall(params)
			if err != nil {
				return nil, err
			}
			return runCaptured(func(out remote.IO) int { return remote.PullMain(ctx, d.Remote, []string{call.ThreadID}, out) })
		},
		methodRemoteGC: func(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
			call, err := decodeRemoteCall(params)
			if err != nil {
				return nil, err
			}
			opts := d.Remote
			opts.Confirm = func(string) (bool, error) { return call.Yes, nil }
			return runCaptured(func(out remote.IO) int { return remote.RunnerMain(ctx, opts, []string{"gc"}, out) })
		},
	}
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
