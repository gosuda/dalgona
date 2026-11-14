package runner

import (
	"bytes"
	"encoding/base64"
	"encoding/json/jsontext"
	"errors"
	"io/fs"
	"maps"
	"slices"
	"strings"
	"time"
)

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

func encodeData(data []byte, next int64) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	encoded := base64.StdEncoding.EncodeToString(data)
	if err := writeString(enc, "dataBase64", encoded); err != nil {
		return nil, err
	}
	if err := writeInt(enc, "next", next); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
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

type readPageParams struct {
	id     string
	path   string
	offset int64
	limit  int64
}

func decodeReadParams(params jsontext.Value) (readPageParams, error) {
	fields, err := paramsFields(params)
	if err != nil {
		return readPageParams{}, err
	}
	id, err := fieldString(fields, "processId")
	if err != nil {
		return readPageParams{}, err
	}
	offset, err := optionalInt(fields, "offset")
	if err != nil {
		return readPageParams{}, err
	}
	limit, err := optionalInt(fields, "limit")
	if err != nil {
		return readPageParams{}, err
	}
	return readPageParams{id: id, offset: offset, limit: clampPage(limit)}, nil
}

func decodeReadFileParams(params jsontext.Value) (readPageParams, error) {
	fields, err := paramsFields(params)
	if err != nil {
		return readPageParams{}, err
	}
	path, err := fieldString(fields, "path")
	if err != nil {
		return readPageParams{}, err
	}
	offset, err := optionalInt(fields, "offset")
	if err != nil {
		return readPageParams{}, err
	}
	limit, err := optionalInt(fields, "limit")
	if err != nil {
		return readPageParams{}, err
	}
	return readPageParams{path: path, offset: offset, limit: clampPage(limit)}, nil
}

func clampPage(limit int64) int64 {
	if limit <= 0 || limit > readPageSize {
		return readPageSize
	}
	return limit
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
	path   string
	data   []byte
	mode   fs.FileMode
	offset int64
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
	offset, err := optionalInt(fields, "offset")
	if err != nil {
		return writeFileParams{}, err
	}
	if offset < 0 {
		return writeFileParams{}, errInvalidParams
	}
	return writeFileParams{path: path, data: data, mode: mode, offset: offset}, nil
}

func decodeRenameParams(params jsontext.Value) (string, string, error) {
	fields, err := paramsFields(params)
	if err != nil {
		return "", "", err
	}
	from, err := fieldString(fields, "from")
	if err != nil {
		return "", "", err
	}
	to, err := fieldString(fields, "to")
	if err != nil {
		return "", "", err
	}
	return from, to, nil
}

func encodeRenameParams(from, to string) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeString(enc, "from", from); err != nil {
		return nil, err
	}
	if err := writeString(enc, "to", to); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
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
