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
	"runtime/debug"
	"sync"
)

type Remote struct {
	conn       io.ReadWriteCloser
	writeMu    sync.Mutex
	mu         sync.Mutex
	next       int64
	pending    map[int64]chan rpcResult
	closed     bool
	done       chan struct{}
	readerDone chan struct{}
	shutdown   func()
}

type rpcResult struct {
	value jsontext.Value
	err   error
}

type processMissingError string

func (e processMissingError) Error() string { return string(e) }

func (e processMissingError) Unwrap() error { return ErrProcessNotFound }

type fileMissingError string

func (e fileMissingError) Error() string { return string(e) }

func (e fileMissingError) Unwrap() error { return fs.ErrNotExist }

var _ Runner = (*Remote)(nil)

func (*Remote) RemoteHost() bool { return true }

func NewRemote(ctx context.Context, boxID string, conn io.ReadWriteCloser) (*Remote, error) {
	remote := &Remote{
		conn:       conn,
		pending:    make(map[int64]chan rpcResult),
		done:       make(chan struct{}),
		readerDone: make(chan struct{}),
	}
	remote.shutdown = sync.OnceFunc(remote.stop)
	go remote.readLoop()
	version, err := remote.initialize(ctx)
	if err != nil {
		_ = remote.Close()
		return nil, err
	}
	local := binaryVersion()
	if version != local {
		_ = remote.Close()
		return nil, versionMismatch(boxID, version, local)
	}
	return remote, nil
}

func versionMismatch(boxID, remoteVersion, localVersion string) error {
	return fmt.Errorf("runner: box %s runs dalgona %s; this is %s; use the matching box image", boxID, remoteVersion, localVersion)
}

func binaryVersion() string {
	info, ok := debug.ReadBuildInfo()
	if !ok || info.Main.Version == "" {
		return "(devel)"
	}
	return info.Main.Version
}

func (r *Remote) initialize(ctx context.Context) (string, error) {
	params, err := encodeClientName()
	if err != nil {
		return "", err
	}
	result, err := r.call(ctx, "initialize", params)
	if err != nil {
		return "", err
	}
	return executorVersion(result)
}

func encodeClientName() (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeString(enc, "clientName", "dalgona"); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func executorVersion(raw jsontext.Value) (string, error) {
	fields, err := objectFields(raw)
	if err != nil {
		return "", err
	}
	info, ok := fields["environmentInfo"]
	if !ok {
		return "", errors.New("runner: missing executor version")
	}
	nested, err := objectFields(info)
	if err != nil {
		return "", err
	}
	return fieldString(nested, "executorVersion")
}

func (r *Remote) Start(ctx context.Context, spec ProcessSpec) error {
	spec.Sandbox = Sandbox{Mode: SandboxOff}
	params, err := encodeProcessSpec(spec)
	if err != nil {
		return err
	}
	_, err = r.call(ctx, "process/start", params)
	return err
}

func (r *Remote) Read(ctx context.Context, id string, offset int64) (Output, error) {
	var combined []byte
	next := offset
	var last Output
	for {
		page, err := r.readPage(ctx, id, next)
		if err != nil {
			return Output{}, err
		}
		combined = append(combined, page.Data...)
		next = page.Next
		last = page
		if int64(len(page.Data)) < readPageSize {
			break
		}
	}
	last.Data = combined
	last.Next = next
	return last, nil
}

func (r *Remote) readPage(ctx context.Context, id string, offset int64) (Output, error) {
	params, err := encodeReadParams(id, offset, readPageSize)
	if err != nil {
		return Output{}, err
	}
	result, err := r.call(ctx, "process/read", params)
	if err != nil {
		return Output{}, err
	}
	return decodeOutput(result)
}

func encodeReadParams(id string, offset, limit int64) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeString(enc, "processId", id); err != nil {
		return nil, err
	}
	if err := writeInt(enc, "offset", offset); err != nil {
		return nil, err
	}
	if err := writeInt(enc, "limit", limit); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func (r *Remote) Signal(ctx context.Context, id string, signal Signal) error {
	name, err := signalName(signal)
	if err != nil {
		return err
	}
	params, err := encodeSignalParams(id, name)
	if err != nil {
		return err
	}
	_, err = r.call(ctx, "process/signal", params)
	return err
}

func encodeSignalParams(id, name string) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeString(enc, "processId", id); err != nil {
		return nil, err
	}
	if err := writeString(enc, "signal", name); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func (r *Remote) ReadFile(ctx context.Context, path string) ([]byte, error) {
	var combined []byte
	var offset int64
	for {
		params, err := encodeReadFileParams(path, offset, readPageSize)
		if err != nil {
			return nil, err
		}
		result, err := r.call(ctx, "fs/readFile", params)
		if err != nil {
			return nil, err
		}
		data, next, err := decodeData(result)
		if err != nil {
			return nil, err
		}
		combined = append(combined, data...)
		if int64(len(data)) < readPageSize {
			return combined, nil
		}
		offset = next
	}
}

func encodeReadFileParams(path string, offset, limit int64) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeString(enc, "path", path); err != nil {
		return nil, err
	}
	if err := writeInt(enc, "offset", offset); err != nil {
		return nil, err
	}
	if err := writeInt(enc, "limit", limit); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func (r *Remote) WriteFile(ctx context.Context, path string, data []byte, perm fs.FileMode) error {
	var offset int64
	for {
		end := min(offset+readPageSize, int64(len(data)))
		params, err := encodeWriteParams(path, data[offset:end], perm, offset)
		if err != nil {
			return err
		}
		if _, err := r.call(ctx, "fs/writeFile", params); err != nil {
			return err
		}
		if end == int64(len(data)) {
			return nil
		}
		offset = end
	}
}

func (r *Remote) Stat(ctx context.Context, path string) (FileInfo, error) {
	params, err := pathParams(path)
	if err != nil {
		return FileInfo{}, err
	}
	result, err := r.call(ctx, "fs/getMetadata", params)
	if err != nil {
		return FileInfo{}, err
	}
	return decodeFileInfo(result)
}

func (r *Remote) ReadDir(ctx context.Context, path string) ([]FileInfo, error) {
	params, err := pathParams(path)
	if err != nil {
		return nil, err
	}
	result, err := r.call(ctx, "fs/readDirectory", params)
	if err != nil {
		return nil, err
	}
	return decodeFileInfos(result)
}

func (r *Remote) Remove(ctx context.Context, path string) error {
	params, err := pathParams(path)
	if err != nil {
		return err
	}
	_, err = r.call(ctx, "fs/remove", params)
	return err
}

func pathParams(path string) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeString(enc, "path", path); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func encodeWriteParams(path string, data []byte, perm fs.FileMode, offset int64) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeString(enc, "path", path); err != nil {
		return nil, err
	}
	encoded := base64.StdEncoding.EncodeToString(data)
	if err := writeString(enc, "dataBase64", encoded); err != nil {
		return nil, err
	}
	if err := writeInt(enc, "mode", int64(perm)); err != nil {
		return nil, err
	}
	if err := writeInt(enc, "offset", offset); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func decodeData(raw jsontext.Value) ([]byte, int64, error) {
	fields, err := objectFields(raw)
	if err != nil {
		return nil, 0, err
	}
	encoded, err := fieldString(fields, "dataBase64")
	if err != nil {
		return nil, 0, err
	}
	data, err := base64.StdEncoding.DecodeString(encoded)
	if err != nil {
		return nil, 0, errors.New("runner: invalid dataBase64")
	}
	next, err := optionalInt(fields, "next")
	if err != nil {
		return nil, 0, err
	}
	return data, next, nil
}

func (r *Remote) Close() error {
	r.shutdown()
	<-r.readerDone
	return nil
}

func (r *Remote) stop() {
	r.failPending()
	_ = r.conn.Close()
}

func (r *Remote) call(ctx context.Context, method string, params jsontext.Value) (jsontext.Value, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	id, ch, err := r.begin()
	if err != nil {
		return nil, err
	}
	payload, err := encodeRequest(id, method, params)
	if err != nil {
		r.cancel(id)
		return nil, err
	}
	if err := r.writePayload(payload); err != nil {
		r.cancel(id)
		r.shutdown()
		return nil, err
	}
	return r.wait(ctx, id, ch)
}

func (r *Remote) wait(ctx context.Context, id int64, ch chan rpcResult) (jsontext.Value, error) {
	select {
	case <-ctx.Done():
		r.cancel(id)
		return nil, ctx.Err()
	case <-r.done:
		return nil, errRemoteClosed
	case res := <-ch:
		return res.value, res.err
	}
}

func (r *Remote) begin() (int64, chan rpcResult, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return 0, nil, errRemoteClosed
	}
	r.next++
	ch := make(chan rpcResult, 1)
	r.pending[r.next] = ch
	return r.next, ch, nil
}

func (r *Remote) cancel(id int64) {
	r.mu.Lock()
	delete(r.pending, id)
	r.mu.Unlock()
}

func (r *Remote) writePayload(payload []byte) error {
	payload = append(payload, '\n')
	r.writeMu.Lock()
	defer r.writeMu.Unlock()
	return writeFull(r.conn, payload)
}

func (r *Remote) readLoop() {
	defer close(r.readerDone)
	defer r.shutdown()
	reader := bufio.NewReaderSize(r.conn, 32<<10)
	for {
		line, err := readLimitedLine(reader, maxRPCLine)
		if err != nil {
			return
		}
		if len(bytes.TrimSpace(line)) == 0 {
			continue
		}
		if err := r.deliver(line); err != nil {
			return
		}
	}
}

func (r *Remote) deliver(line []byte) error {
	msg, err := parseRPC(line)
	if err != nil {
		return err
	}
	if msg.method != "" || !msg.hasID {
		return nil
	}
	id, err := valueInt(msg.id)
	if err != nil {
		return nil
	}
	ch := r.take(id)
	if ch == nil {
		return nil
	}
	ch <- rpcResult{value: msg.result, err: clientError(msg)}
	return nil
}

func (r *Remote) take(id int64) chan rpcResult {
	r.mu.Lock()
	defer r.mu.Unlock()
	ch := r.pending[id]
	delete(r.pending, id)
	return ch
}

func clientError(msg rpcMessage) error {
	if !msg.hasErr {
		return nil
	}
	if msg.errCode == rpcProcessMissing || msg.errMsg == ErrProcessNotFound.Error() {
		return processMissingError(ErrProcessNotFound.Error())
	}
	if msg.errCode == rpcFileMissing {
		return fileMissingError(msg.errMsg)
	}
	if msg.errMsg == "" {
		return errors.New("runner: rpc error")
	}
	return errors.New(msg.errMsg)
}

func (r *Remote) failPending() {
	r.mu.Lock()
	r.closed = true
	pending := r.pending
	r.pending = map[int64]chan rpcResult{}
	r.mu.Unlock()
	for _, ch := range pending {
		ch <- rpcResult{err: errRemoteClosed}
	}
	close(r.done)
}
