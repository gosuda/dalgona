package runner

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"sync"
)

type execPolicy struct {
	boxChannel bool
	floor      SandboxMode
}

func trustedExecPolicy() execPolicy { return execPolicy{} }

func serveBoxChannel(ctx context.Context, conn io.ReadWriteCloser, local *Local, floor SandboxMode) error {
	return serveExecRPC(ctx, conn, local, execPolicy{boxChannel: true, floor: floor})
}

func serveExecRPC(ctx context.Context, conn io.ReadWriteCloser, local *Local, policy execPolicy) error {
	closeConn := sync.OnceFunc(func() { _ = conn.Close() })
	stop := context.AfterFunc(ctx, closeConn)
	defer stop()
	defer closeConn()
	reader := bufio.NewReaderSize(conn, 32<<10)
	writer := rpcWriter{w: conn, mu: &sync.Mutex{}}
	for {
		line, err := readLimitedLine(reader, maxRPCLine)
		if err != nil {
			return serveReadError(ctx, err)
		}
		if len(bytes.TrimSpace(line)) == 0 {
			continue
		}
		if err := dispatchRPC(ctx, writer, local, line, policy); err != nil {
			return err
		}
	}
}

func serveReadError(ctx context.Context, err error) error {
	if ctx.Err() != nil {
		return ctx.Err()
	}
	if errors.Is(err, io.EOF) {
		return nil
	}
	return err
}

func dispatchRPC(ctx context.Context, writer rpcWriter, local *Local, line []byte, policy execPolicy) error {
	msg, err := parseRPC(line)
	if err != nil {
		return writeRPCError(writer, nil, rpcParseError, "parse error")
	}
	if msg.method == "" {
		return nil
	}
	result, err := callMethod(ctx, local, msg.method, msg.params, policy)
	if !msg.hasID {
		return nil
	}
	if err != nil {
		code, text := rpcStatus(err)
		return writeRPCError(writer, msg.id, code, text)
	}
	return writeRPCResult(writer, msg.id, result)
}

func callMethod(ctx context.Context, local *Local, method string, params jsontext.Value, policy execPolicy) (jsontext.Value, error) {
	if method == "process/start" && policy.boxChannel {
		if err := checkSandboxFloor(params, policy.floor); err != nil {
			return nil, err
		}
	}
	handler, ok := rpcMethods[method]
	if !ok {
		return nil, errMethodNotFound
	}
	return handler(ctx, local, params)
}

func checkSandboxFloor(params jsontext.Value, floor SandboxMode) error {
	if floor == SandboxOff {
		return nil
	}
	spec, err := decodeProcessSpec(params)
	if err != nil {
		return err
	}
	if spec.Sandbox.Mode == SandboxWorkspaceWrite {
		return nil
	}
	return ErrSandboxDowngrade
}

func rpcStatus(err error) (int, string) {
	if status, ok := errors.AsType[rpcStatusError](err); ok {
		return status.code, status.msg
	}
	if errors.Is(err, ErrSandboxDowngrade) {
		return rpcInvalidParams, err.Error()
	}
	if errors.Is(err, ErrProcessNotFound) {
		return rpcProcessMissing, ErrProcessNotFound.Error()
	}
	if errors.Is(err, fs.ErrNotExist) {
		return rpcFileMissing, err.Error()
	}
	return rpcInternal, err.Error()
}

var rpcMethods = map[string]func(context.Context, *Local, jsontext.Value) (jsontext.Value, error){
	"initialize":        rpcInitialize,
	"process/start":     rpcStart,
	"process/read":      rpcRead,
	"process/signal":    rpcSignal,
	"process/terminate": rpcTerminate,
	"fs/readFile":       rpcReadFile,
	"fs/writeFile":      rpcWriteFile,
	"fs/rename":         rpcRename,
	"fs/getMetadata":    rpcMetadata,
	"fs/readDirectory":  rpcReadDir,
	"fs/remove":         rpcRemove,
}

func rpcInitialize(context.Context, *Local, jsontext.Value) (jsontext.Value, error) {
	return encodeInitialize()
}

func rpcStart(ctx context.Context, local *Local, params jsontext.Value) (jsontext.Value, error) {
	spec, err := decodeProcessSpec(params)
	if err != nil {
		return nil, err
	}
	if err := local.Start(ctx, spec); err != nil {
		return nil, err
	}
	return emptyObject(), nil
}

func rpcRead(ctx context.Context, local *Local, params jsontext.Value) (jsontext.Value, error) {
	page, err := decodeReadParams(params)
	if err != nil {
		return nil, err
	}
	out, err := local.readPage(ctx, page.id, page.offset, page.limit)
	if err != nil {
		return nil, err
	}
	return encodeOutput(out)
}

func rpcSignal(ctx context.Context, local *Local, params jsontext.Value) (jsontext.Value, error) {
	id, signal, err := decodeSignalParams(params)
	if err != nil {
		return nil, err
	}
	if err := local.Signal(ctx, id, signal); err != nil {
		return nil, err
	}
	return emptyObject(), nil
}

func rpcTerminate(ctx context.Context, local *Local, params jsontext.Value) (jsontext.Value, error) {
	id, err := requireProcessID(params)
	if err != nil {
		return nil, err
	}
	if signalErr := local.Signal(ctx, id, SignalKill); signalErr != nil {
		return nil, signalErr
	}
	state, err := local.Read(ctx, id, 0)
	if err != nil {
		return nil, err
	}
	return encodeRunning(state.Alive)
}

func rpcReadFile(ctx context.Context, local *Local, params jsontext.Value) (jsontext.Value, error) {
	page, err := decodeReadFileParams(params)
	if err != nil {
		return nil, err
	}
	data, next, err := local.readFileAt(ctx, page.path, page.offset, page.limit)
	if err != nil {
		return nil, err
	}
	return encodeData(data, next)
}

func rpcWriteFile(ctx context.Context, local *Local, params jsontext.Value) (jsontext.Value, error) {
	write, err := decodeWriteFile(params)
	if err != nil {
		return nil, err
	}
	if write.offset == 0 {
		if writeErr := local.WriteFile(ctx, write.path, write.data, write.mode); writeErr != nil {
			return nil, writeErr
		}
	} else if writeErr := local.writeFileAt(ctx, write.path, write.data, write.offset, write.mode); writeErr != nil {
		return nil, writeErr
	}
	if syncErr := syncFS(); syncErr != nil {
		return nil, fmt.Errorf("runner: sync filesystem: %w", syncErr)
	}
	return emptyObject(), nil
}

func rpcRename(ctx context.Context, _ *Local, params jsontext.Value) (jsontext.Value, error) {
	from, to, err := decodeRenameParams(params)
	if err != nil {
		return nil, err
	}
	if err = ctx.Err(); err != nil {
		return nil, fmt.Errorf("runner: rename: %w", err)
	}
	if err = publishStagedRename(from, to); err != nil {
		return nil, err
	}
	if err := syncDir(filepath.Dir(to)); err != nil {
		return nil, fmt.Errorf("runner: sync parent: %w", err)
	}
	if syncErr := syncFS(); syncErr != nil {
		return nil, fmt.Errorf("runner: sync filesystem: %w", syncErr)
	}
	return emptyObject(), nil
}

func publishStagedRename(from, to string) error {
	info, err := os.Lstat(from)
	if err != nil {
		return fmt.Errorf("runner: rename %s: %w", from, err)
	}
	if err := publishReplace(from, to, info.Mode().Perm()); err != nil {
		return fmt.Errorf("runner: rename %s: %w", from, err)
	}
	if from != to {
		if _, err := os.Lstat(from); err == nil {
			if err := os.Remove(from); err != nil {
				return fmt.Errorf("runner: remove staged file %s: %w", from, err)
			}
		}
	}
	return nil
}

func rpcMetadata(ctx context.Context, local *Local, params jsontext.Value) (jsontext.Value, error) {
	path, err := requirePath(params)
	if err != nil {
		return nil, err
	}
	info, err := local.Stat(ctx, path)
	if err != nil {
		return nil, err
	}
	return encodeFileInfo(info)
}

func rpcReadDir(ctx context.Context, local *Local, params jsontext.Value) (jsontext.Value, error) {
	path, err := requirePath(params)
	if err != nil {
		return nil, err
	}
	entries, err := local.ReadDir(ctx, path)
	if err != nil {
		return nil, err
	}
	return encodeFileInfos(entries)
}

func rpcRemove(ctx context.Context, local *Local, params jsontext.Value) (jsontext.Value, error) {
	path, err := requirePath(params)
	if err != nil {
		return nil, err
	}
	if err := local.Remove(ctx, path); err != nil {
		return nil, err
	}
	return emptyObject(), nil
}
