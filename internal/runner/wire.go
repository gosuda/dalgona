package runner

import (
	"bufio"
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"maps"
	"slices"
	"strings"
	"sync"
	"time"
)

const maxRPCLine = 16 << 20

const (
	rpcParseError     = -32700
	rpcInvalidParams  = -32602
	rpcMethodNotFound = -32601
	rpcInternal       = -32000
	rpcProcessMissing = -32001
	rpcFileMissing    = -32002
)

// ErrSandboxDowngrade reports a box-channel request that weakens the
// sandbox below the server start mode.
var ErrSandboxDowngrade = errors.New("runner: sandbox downgrade refused")

var (
	errParse          = errors.New("parse error")
	errInvalidParams  = rpcStatusError{code: rpcInvalidParams, msg: "invalid params"}
	errMethodNotFound = rpcStatusError{code: rpcMethodNotFound, msg: "method not found"}
	errLineTooLong    = errors.New("runner: rpc line exceeds 16MiB")
	errRemoteClosed   = errors.New("runner: remote closed")
)

type rpcStatusError struct {
	code int
	msg  string
}

func (e rpcStatusError) Error() string { return e.msg }

type rpcMessage struct {
	hasID   bool
	id      jsontext.Value
	method  string
	params  jsontext.Value
	result  jsontext.Value
	hasErr  bool
	errCode int
	errMsg  string
}

type rpcWriter struct {
	w  io.Writer
	mu *sync.Mutex
}

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
	id, offset, err := decodeReadParams(params)
	if err != nil {
		return nil, err
	}
	out, err := local.Read(ctx, id, offset)
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

func encodeRunning(running bool) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeBool(enc, "running", running); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func rpcReadFile(ctx context.Context, local *Local, params jsontext.Value) (jsontext.Value, error) {
	path, err := requirePath(params)
	if err != nil {
		return nil, err
	}
	data, err := local.ReadFile(ctx, path)
	if err != nil {
		return nil, err
	}
	return encodeData(data)
}

func rpcWriteFile(ctx context.Context, local *Local, params jsontext.Value) (jsontext.Value, error) {
	write, err := decodeWriteFile(params)
	if err != nil {
		return nil, err
	}
	if writeErr := local.WriteFile(ctx, write.path, write.data, write.mode); writeErr != nil {
		return nil, writeErr
	}
	if syncErr := syncFS(); syncErr != nil {
		return nil, fmt.Errorf("runner: sync filesystem: %w", syncErr)
	}
	return emptyObject(), nil
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

func readLimitedLine(r *bufio.Reader, limit int) ([]byte, error) {
	var out []byte
	for {
		frag, err := r.ReadSlice('\n')
		if len(out)+len(frag) > limit+1 {
			return nil, errLineTooLong
		}
		out = append(out, frag...)
		if err == nil {
			return trimNL(out), nil
		}
		if !errors.Is(err, bufio.ErrBufferFull) {
			return nil, lineEndError(out, err)
		}
	}
}

func lineEndError(out []byte, err error) error {
	if errors.Is(err, io.EOF) && len(out) == 0 {
		return io.EOF
	}
	if errors.Is(err, io.EOF) {
		return io.ErrUnexpectedEOF
	}
	return err
}

func trimNL(line []byte) []byte {
	line = bytes.TrimSuffix(line, []byte("\n"))
	return bytes.TrimSuffix(line, []byte("\r"))
}

func parseRPC(line []byte) (rpcMessage, error) {
	dec := jsontext.NewDecoder(bytes.NewReader(line))
	tok, err := dec.ReadToken()
	if err != nil || tok.Kind() != jsontext.KindBeginObject {
		return rpcMessage{}, errParse
	}
	var msg rpcMessage
	for dec.PeekKind() != jsontext.KindEndObject {
		if err := readRPCField(dec, &msg); err != nil {
			return rpcMessage{}, err
		}
	}
	if _, err := dec.ReadToken(); err != nil {
		return rpcMessage{}, errParse
	}
	if _, err := dec.ReadValue(); !errors.Is(err, io.EOF) {
		return rpcMessage{}, errParse
	}
	return msg, nil
}

func readRPCField(dec *jsontext.Decoder, msg *rpcMessage) error {
	key, err := dec.ReadToken()
	if err != nil || key.Kind() != jsontext.KindString {
		return errParse
	}
	return applyRPCField(dec, msg, key.String())
}

func applyRPCField(dec *jsontext.Decoder, msg *rpcMessage, key string) error {
	switch key {
	case "id":
		return readRPCID(dec, msg)
	case "method":
		return readRPCMethod(dec, msg)
	case "params":
		return readRPCValue(dec, &msg.params)
	case "result":
		return readRPCValue(dec, &msg.result)
	case "error":
		return readRPCError(dec, msg)
	default:
		return skipValue(dec)
	}
}

func readRPCID(dec *jsontext.Decoder, msg *rpcMessage) error {
	raw, err := dec.ReadValue()
	if err != nil {
		return errParse
	}
	msg.hasID = true
	msg.id = raw.Clone()
	return nil
}

func readRPCMethod(dec *jsontext.Decoder, msg *rpcMessage) error {
	raw, err := dec.ReadValue()
	if err != nil {
		return errParse
	}
	text, err := valueString(raw)
	if err != nil {
		return errParse
	}
	msg.method = text
	return nil
}

func readRPCValue(dec *jsontext.Decoder, dst *jsontext.Value) error {
	raw, err := dec.ReadValue()
	if err != nil {
		return errParse
	}
	*dst = raw.Clone()
	return nil
}

func readRPCError(dec *jsontext.Decoder, msg *rpcMessage) error {
	raw, err := dec.ReadValue()
	if err != nil {
		return errParse
	}
	fields, err := objectFields(raw)
	if err != nil {
		return errParse
	}
	code, err := valueInt(fields["code"])
	if err != nil {
		return errParse
	}
	text, err := valueString(fields["message"])
	if err != nil {
		return errParse
	}
	msg.hasErr = true
	msg.errCode = int(code)
	msg.errMsg = text
	return nil
}

func skipValue(dec *jsontext.Decoder) error {
	if _, err := dec.ReadValue(); err != nil {
		return errParse
	}
	return nil
}

func writeRPCResult(writer rpcWriter, id, result jsontext.Value) error {
	payload, err := encodeEnvelope(id, "result", resultOrEmpty(result))
	if err != nil {
		return err
	}
	return writePayload(writer, payload)
}

func writeRPCError(writer rpcWriter, id jsontext.Value, code int, msg string) error {
	body, err := encodeErrorBody(code, msg)
	if err != nil {
		return err
	}
	payload, err := encodeEnvelope(id, "error", body)
	if err != nil {
		return err
	}
	return writePayload(writer, payload)
}

func writePayload(writer rpcWriter, payload []byte) error {
	payload = append(payload, '\n')
	writer.mu.Lock()
	defer writer.mu.Unlock()
	return writeFull(writer.w, payload)
}

func writeFull(w io.Writer, payload []byte) error {
	for len(payload) > 0 {
		n, err := w.Write(payload)
		payload = payload[n:]
		if err != nil {
			return err
		}
		if n == 0 {
			return io.ErrShortWrite
		}
	}
	return nil
}

func encodeEnvelope(id jsontext.Value, field string, body jsontext.Value) ([]byte, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeString(enc, "jsonrpc", "2.0"); err != nil {
		return nil, err
	}
	if err := writeRaw(enc, "id", idOrNull(id)); err != nil {
		return nil, err
	}
	if err := writeRaw(enc, field, body); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func encodeErrorBody(code int, msg string) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeInt(enc, "code", int64(code)); err != nil {
		return nil, err
	}
	if err := writeString(enc, "message", msg); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func encodeRequest(id int64, method string, params jsontext.Value) ([]byte, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeString(enc, "jsonrpc", "2.0"); err != nil {
		return nil, err
	}
	if err := writeInt(enc, "id", id); err != nil {
		return nil, err
	}
	if err := writeString(enc, "method", method); err != nil {
		return nil, err
	}
	if err := writeRaw(enc, "params", resultOrEmpty(params)); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func encodeInitialize() (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeEnvironment(enc); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func writeEnvironment(enc *jsontext.Encoder) error {
	if err := enc.WriteToken(jsontext.String("environmentInfo")); err != nil {
		return err
	}
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	if err := writeString(enc, "executorVersion", binaryVersion()); err != nil {
		return err
	}
	return enc.WriteToken(jsontext.EndObject)
}

func encodeOutput(out Output) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeOutputFields(enc, out); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func writeOutputFields(enc *jsontext.Encoder, out Output) error {
	encoded := base64.StdEncoding.EncodeToString(out.Data)
	if err := writeString(enc, "dataBase64", encoded); err != nil {
		return err
	}
	if err := writeInt(enc, "next", out.Next); err != nil {
		return err
	}
	if err := writeExitField(enc, out.Exit); err != nil {
		return err
	}
	if err := writeBool(enc, "alive", out.Alive); err != nil {
		return err
	}
	return writeString(enc, "path", out.Path)
}

func writeExitField(enc *jsontext.Encoder, exit *Exit) error {
	if err := enc.WriteToken(jsontext.String("exit")); err != nil {
		return err
	}
	if exit == nil {
		return enc.WriteToken(jsontext.Null)
	}
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	if err := writeExitFields(enc, exit); err != nil {
		return err
	}
	return enc.WriteToken(jsontext.EndObject)
}

func writeExitFields(enc *jsontext.Encoder, exit *Exit) error {
	if err := writeInt(enc, "code", int64(exit.Code)); err != nil {
		return err
	}
	if err := writeString(enc, "signal", exit.Signal); err != nil {
		return err
	}
	return writeInt(enc, "endedAtMs", exit.EndedAt.UnixMilli())
}

func encodeData(data []byte) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	encoded := base64.StdEncoding.EncodeToString(data)
	if err := writeString(enc, "dataBase64", encoded); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func encodeFileInfo(info FileInfo) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeFileInfoFields(enc, info); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func encodeFileInfos(entries []FileInfo) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeFileInfoList(enc, entries); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func writeFileInfoList(enc *jsontext.Encoder, entries []FileInfo) error {
	if err := enc.WriteToken(jsontext.String("entries")); err != nil {
		return err
	}
	if err := enc.WriteToken(jsontext.BeginArray); err != nil {
		return err
	}
	for _, info := range entries {
		if err := writeFileInfoObject(enc, info); err != nil {
			return err
		}
	}
	return enc.WriteToken(jsontext.EndArray)
}

func writeFileInfoObject(enc *jsontext.Encoder, info FileInfo) error {
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	if err := writeFileInfoFields(enc, info); err != nil {
		return err
	}
	return enc.WriteToken(jsontext.EndObject)
}

func writeFileInfoFields(enc *jsontext.Encoder, info FileInfo) error {
	if err := writeString(enc, "name", info.Name); err != nil {
		return err
	}
	if err := writeInt(enc, "size", info.Size); err != nil {
		return err
	}
	if err := writeInt(enc, "mode", int64(info.Mode)); err != nil {
		return err
	}
	if err := writeInt(enc, "modTimeMs", info.ModTime.UnixMilli()); err != nil {
		return err
	}
	if err := writeBool(enc, "isDirectory", info.Mode.IsDir()); err != nil {
		return err
	}
	return writeBool(enc, "isFile", info.Mode.IsRegular())
}

func encodeProcessSpec(spec ProcessSpec) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeProcessFields(enc, spec); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func writeProcessFields(enc *jsontext.Encoder, spec ProcessSpec) error {
	if err := writeString(enc, "processId", spec.ID); err != nil {
		return err
	}
	if err := writeStringArray(enc, "argv", spec.Argv); err != nil {
		return err
	}
	if err := writeString(enc, "cwd", spec.Dir); err != nil {
		return err
	}
	if err := writeEnv(enc, spec.Env); err != nil {
		return err
	}
	return writeSandbox(enc, spec.Sandbox)
}

func writeEnv(enc *jsontext.Encoder, env []string) error {
	if err := enc.WriteToken(jsontext.String("env")); err != nil {
		return err
	}
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	for _, item := range envPairs(env) {
		if err := writeString(enc, item[0], item[1]); err != nil {
			return err
		}
	}
	return enc.WriteToken(jsontext.EndObject)
}

func envPairs(env []string) [][2]string {
	values := make(map[string]string, len(env))
	for _, item := range env {
		key, val, _ := strings.Cut(item, "=")
		values[key] = val
	}
	keys := slices.Sorted(maps.Keys(values))
	out := make([][2]string, len(keys))
	for i, key := range keys {
		out[i] = [2]string{key, values[key]}
	}
	return out
}

func writeSandbox(enc *jsontext.Encoder, sandbox Sandbox) error {
	if err := enc.WriteToken(jsontext.String("sandbox")); err != nil {
		return err
	}
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	if err := writeString(enc, "mode", string(sandbox.Mode)); err != nil {
		return err
	}
	if err := writeStringArray(enc, "writableRoots", sandbox.WritableRoots); err != nil {
		return err
	}
	return enc.WriteToken(jsontext.EndObject)
}

func writeStringArray(enc *jsontext.Encoder, key string, values []string) error {
	if err := enc.WriteToken(jsontext.String(key)); err != nil {
		return err
	}
	if err := enc.WriteToken(jsontext.BeginArray); err != nil {
		return err
	}
	for _, value := range values {
		if err := enc.WriteToken(jsontext.String(value)); err != nil {
			return err
		}
	}
	return enc.WriteToken(jsontext.EndArray)
}

func decodeProcessSpec(params jsontext.Value) (ProcessSpec, error) {
	fields, err := paramsFields(params)
	if err != nil {
		return ProcessSpec{}, err
	}
	id, err := fieldString(fields, "processId")
	if err != nil {
		return ProcessSpec{}, err
	}
	argv, err := fieldStrings(fields, "argv")
	if err != nil {
		return ProcessSpec{}, err
	}
	return finishProcessSpec(fields, id, argv)
}

func finishProcessSpec(fields map[string]jsontext.Value, id string, argv []string) (ProcessSpec, error) {
	dir, err := optionalString(fields, "cwd")
	if err != nil {
		return ProcessSpec{}, err
	}
	env, err := optionalEnv(fields)
	if err != nil {
		return ProcessSpec{}, err
	}
	sandbox, err := optionalSandbox(fields)
	if err != nil {
		return ProcessSpec{}, err
	}
	return ProcessSpec{ID: id, Argv: argv, Dir: dir, Env: env, Sandbox: sandbox}, nil
}

func decodeReadParams(params jsontext.Value) (string, int64, error) {
	fields, err := paramsFields(params)
	if err != nil {
		return "", 0, err
	}
	id, err := fieldString(fields, "processId")
	if err != nil {
		return "", 0, err
	}
	offset, err := optionalInt(fields, "offset")
	if err != nil {
		return "", 0, err
	}
	return id, offset, nil
}

func decodeSignalParams(params jsontext.Value) (string, Signal, error) {
	fields, err := paramsFields(params)
	if err != nil {
		return "", 0, err
	}
	id, err := fieldString(fields, "processId")
	if err != nil {
		return "", 0, err
	}
	name, err := fieldString(fields, "signal")
	if err != nil {
		return "", 0, err
	}
	signal, err := decodeSignalName(name)
	if err != nil {
		return "", 0, err
	}
	return id, signal, nil
}

func decodeSignalName(name string) (Signal, error) {
	switch name {
	case "term":
		return SignalTerm, nil
	case "kill":
		return SignalKill, nil
	default:
		return 0, errInvalidParams
	}
}

func signalName(signal Signal) (string, error) {
	switch signal {
	case SignalTerm:
		return "term", nil
	case SignalKill:
		return "kill", nil
	default:
		return "", errors.New("runner: unknown signal")
	}
}

type writeFileParams struct {
	path string
	data []byte
	mode fs.FileMode
}

func decodeWriteFile(params jsontext.Value) (writeFileParams, error) {
	fields, err := paramsFields(params)
	if err != nil {
		return writeFileParams{}, err
	}
	path, err := fieldString(fields, "path")
	if err != nil {
		return writeFileParams{}, err
	}
	encoded, err := fieldString(fields, "dataBase64")
	if err != nil {
		return writeFileParams{}, err
	}
	return finishWriteFile(fields, path, encoded)
}

func finishWriteFile(fields map[string]jsontext.Value, path, encoded string) (writeFileParams, error) {
	data, err := base64.StdEncoding.DecodeString(encoded)
	if err != nil {
		return writeFileParams{}, errInvalidParams
	}
	mode, err := fieldMode(fields, "mode")
	if err != nil {
		return writeFileParams{}, err
	}
	return writeFileParams{path: path, data: data, mode: mode}, nil
}

func requireProcessID(params jsontext.Value) (string, error) {
	fields, err := paramsFields(params)
	if err != nil {
		return "", err
	}
	return fieldString(fields, "processId")
}

func requirePath(params jsontext.Value) (string, error) {
	fields, err := paramsFields(params)
	if err != nil {
		return "", err
	}
	return fieldString(fields, "path")
}

func decodeOutput(raw jsontext.Value) (Output, error) {
	fields, err := objectFields(raw)
	if err != nil {
		return Output{}, err
	}
	encoded, err := fieldString(fields, "dataBase64")
	if err != nil {
		return Output{}, err
	}
	data, err := base64.StdEncoding.DecodeString(encoded)
	if err != nil {
		return Output{}, errors.New("runner: invalid dataBase64")
	}
	return finishOutput(fields, data)
}

func finishOutput(fields map[string]jsontext.Value, data []byte) (Output, error) {
	next, err := fieldInt(fields, "next")
	if err != nil {
		return Output{}, err
	}
	alive, err := fieldBool(fields, "alive")
	if err != nil {
		return Output{}, err
	}
	path, err := optionalString(fields, "path")
	if err != nil {
		return Output{}, err
	}
	exit, err := optionalExit(fields)
	if err != nil {
		return Output{}, err
	}
	return Output{Data: data, Next: next, Exit: exit, Alive: alive, Path: path}, nil
}

func optionalExit(fields map[string]jsontext.Value) (*Exit, error) {
	raw, ok := fields["exit"]
	if !ok || raw.Kind() == jsontext.KindNull {
		return nil, nil
	}
	return decodeExit(raw)
}

func decodeExit(raw jsontext.Value) (*Exit, error) {
	fields, err := objectFields(raw)
	if err != nil {
		return nil, err
	}
	code, err := fieldInt(fields, "code")
	if err != nil {
		return nil, err
	}
	signal, err := fieldString(fields, "signal")
	if err != nil {
		return nil, err
	}
	ms, err := fieldInt(fields, "endedAtMs")
	if err != nil {
		return nil, err
	}
	exit := Exit{Code: int(code), Signal: signal, EndedAt: time.UnixMilli(ms)}
	return &exit, nil
}

func decodeFileInfo(raw jsontext.Value) (FileInfo, error) {
	fields, err := objectFields(raw)
	if err != nil {
		return FileInfo{}, err
	}
	name, err := fieldString(fields, "name")
	if err != nil {
		return FileInfo{}, err
	}
	size, err := fieldInt(fields, "size")
	if err != nil {
		return FileInfo{}, err
	}
	return finishFileInfo(fields, name, size)
}

func finishFileInfo(fields map[string]jsontext.Value, name string, size int64) (FileInfo, error) {
	mode, err := fieldMode(fields, "mode")
	if err != nil {
		return FileInfo{}, err
	}
	ms, err := fieldInt(fields, "modTimeMs")
	if err != nil {
		return FileInfo{}, err
	}
	info := FileInfo{Name: name, Size: size, Mode: mode, ModTime: time.UnixMilli(ms)}
	return info, nil
}

func decodeFileInfos(raw jsontext.Value) ([]FileInfo, error) {
	fields, err := objectFields(raw)
	if err != nil {
		return nil, err
	}
	entries, ok := fields["entries"]
	if !ok {
		return nil, errInvalidParams
	}
	return valueFileInfos(entries)
}

func valueFileInfos(raw jsontext.Value) ([]FileInfo, error) {
	dec := jsontext.NewDecoder(bytes.NewReader(raw))
	tok, err := dec.ReadToken()
	if err != nil || tok.Kind() != jsontext.KindBeginArray {
		return nil, errInvalidParams
	}
	var out []FileInfo
	for dec.PeekKind() != jsontext.KindEndArray {
		info, err := readFileInfoValue(dec)
		if err != nil {
			return nil, err
		}
		out = append(out, info)
	}
	if _, err := dec.ReadToken(); err != nil {
		return nil, errInvalidParams
	}
	return out, nil
}

func readFileInfoValue(dec *jsontext.Decoder) (FileInfo, error) {
	item, err := dec.ReadValue()
	if err != nil {
		return FileInfo{}, errInvalidParams
	}
	return decodeFileInfo(item)
}

func optionalEnv(fields map[string]jsontext.Value) ([]string, error) {
	raw, ok := fields["env"]
	if !ok || raw.Kind() == jsontext.KindNull {
		return nil, nil
	}
	values, err := valueStringMap(raw)
	if err != nil {
		return nil, err
	}
	return envList(values), nil
}

func envList(values map[string]string) []string {
	keys := slices.Sorted(maps.Keys(values))
	out := make([]string, len(keys))
	for i, key := range keys {
		out[i] = key + "=" + values[key]
	}
	return out
}

func optionalSandbox(fields map[string]jsontext.Value) (Sandbox, error) {
	raw, ok := fields["sandbox"]
	if !ok || raw.Kind() == jsontext.KindNull {
		return Sandbox{}, nil
	}
	return decodeSandbox(raw)
}

func decodeSandbox(raw jsontext.Value) (Sandbox, error) {
	fields, err := objectFields(raw)
	if err != nil {
		return Sandbox{}, err
	}
	mode, err := optionalString(fields, "mode")
	if err != nil {
		return Sandbox{}, err
	}
	roots, err := optionalStrings(fields, "writableRoots")
	if err != nil {
		return Sandbox{}, err
	}
	return Sandbox{Mode: SandboxMode(mode), WritableRoots: roots}, nil
}

func paramsFields(raw jsontext.Value) (map[string]jsontext.Value, error) {
	if len(raw) == 0 || raw.Kind() == jsontext.KindNull {
		return map[string]jsontext.Value{}, nil
	}
	return objectFields(raw)
}

func objectFields(raw jsontext.Value) (map[string]jsontext.Value, error) {
	dec := jsontext.NewDecoder(bytes.NewReader(raw))
	tok, err := dec.ReadToken()
	if err != nil || tok.Kind() != jsontext.KindBeginObject {
		return nil, errInvalidParams
	}
	fields := make(map[string]jsontext.Value)
	for dec.PeekKind() != jsontext.KindEndObject {
		if err := readObjectField(dec, fields); err != nil {
			return nil, err
		}
	}
	if _, err := dec.ReadToken(); err != nil {
		return nil, errInvalidParams
	}
	return fields, nil
}

func readObjectField(dec *jsontext.Decoder, fields map[string]jsontext.Value) error {
	key, err := dec.ReadToken()
	if err != nil || key.Kind() != jsontext.KindString {
		return errInvalidParams
	}
	name := strings.Clone(key.String())
	val, err := dec.ReadValue()
	if err != nil {
		return errInvalidParams
	}
	fields[name] = val.Clone()
	return nil
}

func valueStringMap(raw jsontext.Value) (map[string]string, error) {
	fields, err := objectFields(raw)
	if err != nil {
		return nil, err
	}
	out := make(map[string]string, len(fields))
	for key, val := range fields {
		text, err := valueString(val)
		if err != nil {
			return nil, err
		}
		out[key] = text
	}
	return out, nil
}

func fieldString(fields map[string]jsontext.Value, key string) (string, error) {
	raw, ok := fields[key]
	if !ok {
		return "", errInvalidParams
	}
	return valueString(raw)
}

func optionalString(fields map[string]jsontext.Value, key string) (string, error) {
	raw, ok := fields[key]
	if !ok || raw.Kind() == jsontext.KindNull {
		return "", nil
	}
	return valueString(raw)
}

func fieldStrings(fields map[string]jsontext.Value, key string) ([]string, error) {
	raw, ok := fields[key]
	if !ok {
		return nil, errInvalidParams
	}
	return valueStrings(raw)
}

func optionalStrings(fields map[string]jsontext.Value, key string) ([]string, error) {
	raw, ok := fields[key]
	if !ok || raw.Kind() == jsontext.KindNull {
		return nil, nil
	}
	return valueStrings(raw)
}

func fieldInt(fields map[string]jsontext.Value, key string) (int64, error) {
	raw, ok := fields[key]
	if !ok {
		return 0, errInvalidParams
	}
	return valueInt(raw)
}

func fieldMode(fields map[string]jsontext.Value, key string) (fs.FileMode, error) {
	mode, err := fieldInt(fields, key)
	if err != nil {
		return 0, err
	}
	if mode < 0 || mode > 1<<32-1 {
		return 0, errInvalidParams
	}
	return fs.FileMode(uint32(mode)), nil
}

func optionalInt(fields map[string]jsontext.Value, key string) (int64, error) {
	raw, ok := fields[key]
	if !ok || raw.Kind() == jsontext.KindNull {
		return 0, nil
	}
	return valueInt(raw)
}

func fieldBool(fields map[string]jsontext.Value, key string) (bool, error) {
	raw, ok := fields[key]
	if !ok {
		return false, errInvalidParams
	}
	return valueBool(raw)
}

func valueString(raw jsontext.Value) (string, error) {
	tok, err := oneToken(raw)
	if err != nil || tok.Kind() != jsontext.KindString {
		return "", errInvalidParams
	}
	return tok.String(), nil
}

func valueInt(raw jsontext.Value) (int64, error) {
	tok, err := oneToken(raw)
	if err != nil || tok.Kind() != jsontext.KindNumber {
		return 0, errInvalidParams
	}
	return tok.Int()
}

func valueBool(raw jsontext.Value) (bool, error) {
	tok, err := oneToken(raw)
	if err != nil {
		return false, errInvalidParams
	}
	kind := tok.Kind()
	if kind != jsontext.KindTrue && kind != jsontext.KindFalse {
		return false, errInvalidParams
	}
	return kind == jsontext.KindTrue, nil
}

func valueStrings(raw jsontext.Value) ([]string, error) {
	dec := jsontext.NewDecoder(bytes.NewReader(raw))
	tok, err := dec.ReadToken()
	if err != nil || tok.Kind() != jsontext.KindBeginArray {
		return nil, errInvalidParams
	}
	var out []string
	for dec.PeekKind() != jsontext.KindEndArray {
		text, err := readStringValue(dec)
		if err != nil {
			return nil, err
		}
		out = append(out, text)
	}
	if _, err := dec.ReadToken(); err != nil {
		return nil, errInvalidParams
	}
	return out, nil
}

func readStringValue(dec *jsontext.Decoder) (string, error) {
	item, err := dec.ReadValue()
	if err != nil {
		return "", errInvalidParams
	}
	return valueString(item)
}

func oneToken(raw jsontext.Value) (jsontext.Token, error) {
	dec := jsontext.NewDecoder(bytes.NewReader(raw))
	return dec.ReadToken()
}

func writeString(enc *jsontext.Encoder, key, value string) error {
	if err := enc.WriteToken(jsontext.String(key)); err != nil {
		return err
	}
	return enc.WriteToken(jsontext.String(value))
}

func writeInt(enc *jsontext.Encoder, key string, value int64) error {
	if err := enc.WriteToken(jsontext.String(key)); err != nil {
		return err
	}
	return enc.WriteToken(jsontext.Int(value))
}

func writeBool(enc *jsontext.Encoder, key string, value bool) error {
	if err := enc.WriteToken(jsontext.String(key)); err != nil {
		return err
	}
	return enc.WriteToken(jsontext.Bool(value))
}

func writeRaw(enc *jsontext.Encoder, key string, value jsontext.Value) error {
	if err := enc.WriteToken(jsontext.String(key)); err != nil {
		return err
	}
	return enc.WriteValue(value)
}

func idOrNull(id jsontext.Value) jsontext.Value {
	if len(id) == 0 {
		return jsontext.Value("null")
	}
	return id
}

func resultOrEmpty(result jsontext.Value) jsontext.Value {
	if len(result) == 0 {
		return emptyObject()
	}
	return result
}

func emptyObject() jsontext.Value { return jsontext.Value("{}") }
