package rpcserver

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"math"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

const (
	methodWorkspaceWrite  = "dalgona/workspace/write"
	workspaceUploadLimit  = 4
	workspaceUploadChunk  = 1 << 20
	workspaceUploadTotal  = 64 << 20
	workspaceStagedBytes  = 128 << 20
	workspaceUploadIdle   = 5 * time.Minute
	workspaceDigestLength = 64
)

type workspaceUpload struct {
	handle     *workspaceHandle
	path       string
	expected   string
	digest     string
	total      int64
	data       []byte
	expires    time.Time
	committing bool
}

type workspaceWriteParams struct {
	Stage            string `json:"stage"`
	WorkspaceID      string `json:"workspaceId"`
	Path             string `json:"path"`
	ExpectedRevision string `json:"expectedRevision"`
	Total            string `json:"total"`
	Digest           string `json:"digest"`
	OperationID      string `json:"operationId"`
	UploadID         string `json:"uploadId"`
	Offset           string `json:"offset"`
	DataBase64       []byte `json:"dataBase64"`
}

type workspaceUploadBegun struct {
	UploadID  string `json:"uploadId"`
	Offset    string `json:"offset"`
	ExpiresAt string `json:"expiresAt"`
}

func (s *server) workspaceWrite(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p workspaceWriteParams
	if err := json.Unmarshal(params, &p); err != nil {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	switch p.Stage {
	case "begin":
		return s.writeOperation(ctx, params, p, s.uploadBegin)
	case "chunk":
		return s.uploadChunk(p)
	case "commit":
		return s.writeOperation(ctx, params, p, s.uploadCommit)
	case "abort":
		return s.uploadAbort(p)
	}
	return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
}

func (s *server) writeOperation(ctx context.Context, params jsontext.Value, p workspaceWriteParams, effect func(context.Context, workspaceWriteParams) (jsontext.Value, error)) (jsontext.Value, error) {
	if p.OperationID == "" {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	digest, err := paramDigest(params)
	if err != nil {
		return nil, err
	}
	op := operation{method: methodWorkspaceWrite, id: p.OperationID, digest: digest}
	return s.runOperation(ctx, op, func(ctx context.Context) (jsontext.Value, error) { return effect(ctx, p) })
}

func (p workspaceWriteParams) declaredTotal() (int64, bool) {
	total, err := rpc.ParseDecimal(p.Total)
	if err != nil || total > workspaceUploadTotal || total > math.MaxInt64 {
		return 0, false
	}
	return int64(total), true
}

func validDigest(digest string) bool {
	if len(digest) != workspaceDigestLength {
		return false
	}
	_, err := hex.DecodeString(digest)
	return err == nil && digest == strings.ToLower(digest)
}

func (s *server) uploadBegin(ctx context.Context, p workspaceWriteParams) (jsontext.Value, error) {
	total, ok := p.declaredTotal()
	if p.WorkspaceID == "" || !ok || !validDigest(p.Digest) || !validRevision(p.ExpectedRevision) {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	use, err := s.workspaceUse(ctx, p.WorkspaceID, rightWrite, false)
	if err != nil {
		return nil, err
	}
	defer use.release()
	if pathErr := checkWorkspacePath(p.Path, false, use.handle.pathStyle); pathErr != nil {
		return nil, pathErr
	}
	upload := &workspaceUpload{handle: use.handle, path: p.Path, expected: p.ExpectedRevision, digest: p.Digest, total: total, data: make([]byte, 0, total)}
	id, err := s.workspace.stage(upload)
	if err != nil {
		return nil, err
	}
	return json.Marshal(workspaceUploadBegun{UploadID: id, Offset: "0", ExpiresAt: rpc.FormatTimestamp(upload.expires)})
}

func (t *workspaceTable) stage(upload *workspaceUpload) (string, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.init()
	now := time.Now()
	t.pruneUploads(now)
	if len(t.uploads) >= workspaceUploadLimit || t.staged+upload.total > workspaceStagedBytes {
		return "", rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	if upload.handle.closed {
		return "", rpc.NativeKind(rpc.ErrorExpired)
	}
	upload.expires = now.Add(workspaceUploadIdle)
	id := newHandle()
	t.uploads[id] = upload
	t.staged += upload.total
	return id, nil
}

func (t *workspaceTable) pruneUploads(now time.Time) {
	for id, upload := range t.uploads {
		if !upload.committing && now.After(upload.expires) {
			t.staged -= upload.total
			delete(t.uploads, id)
		}
	}
}

func (t *workspaceTable) upload(id string) (*workspaceUpload, error) {
	t.init()
	t.pruneUploads(time.Now())
	upload := t.uploads[id]
	if upload == nil || upload.handle.closed {
		return nil, rpc.NativeKind(rpc.ErrorExpired)
	}
	if upload.committing {
		return nil, rpc.NativeKind(rpc.ErrorBusy)
	}
	return upload, nil
}

func (s *server) uploadChunk(p workspaceWriteParams) (jsontext.Value, error) {
	offset, err := rpc.ParseDecimal(p.Offset)
	if p.UploadID == "" || err != nil || len(p.DataBase64) > workspaceUploadChunk {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	next, err := s.workspace.append(p.UploadID, int64(min(offset, workspaceUploadTotal+1)), p.DataBase64)
	if err != nil {
		return nil, err
	}
	if next < 0 {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return json.Marshal(struct {
		Next string `json:"next"`
	}{Next: rpc.FormatDecimal(uint64(next))})
}

func (t *workspaceTable) append(id string, offset int64, data []byte) (int64, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	upload, err := t.upload(id)
	if err != nil {
		return 0, err
	}
	received := int64(len(upload.data))
	end := offset + int64(len(data))
	switch {
	case offset < received && end <= received && bytes.Equal(upload.data[offset:end], data):
		return received, nil
	case offset < received:
		return 0, rpc.NativeKind(rpc.ErrorOperationConflict)
	case offset > received || end > upload.total:
		return 0, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	upload.data = append(upload.data, data...)
	upload.expires = time.Now().Add(workspaceUploadIdle)
	upload.handle.used = time.Now()
	return end, nil
}

func (t *workspaceTable) claimCommit(id string) (*workspaceUpload, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	upload, err := t.upload(id)
	if err != nil {
		return nil, err
	}
	sum := sha256.Sum256(upload.data)
	if int64(len(upload.data)) != upload.total || hex.EncodeToString(sum[:]) != upload.digest {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	upload.committing = true
	return upload, nil
}

func (t *workspaceTable) finishUpload(id string, upload *workspaceUpload) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.uploads[id] == upload {
		t.staged -= upload.total
		delete(t.uploads, id)
	}
}

func (s *server) uploadCommit(ctx context.Context, p workspaceWriteParams) (jsontext.Value, error) {
	if p.UploadID == "" {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	upload, err := s.workspace.claimCommit(p.UploadID)
	if err != nil {
		return nil, err
	}
	defer s.workspace.finishUpload(p.UploadID, upload)
	use, err := s.workspaceUse(ctx, upload.handle.id, rightWrite, true)
	if err != nil {
		return nil, err
	}
	defer use.release()
	entry, err := s.publishUpload(use, upload)
	if err != nil {
		return nil, err
	}
	return json.Marshal(workspaceMetadataResult{Metadata: metadataOf(entry)})
}

func (s *server) publishUpload(use workspaceUse, upload *workspaceUpload) (runner.WorkspaceEntry, error) {
	staging := ""
	for offset := 0; offset == 0 || offset < len(upload.data); offset += workspaceUploadChunk {
		chunk := upload.data[offset:min(offset+workspaceUploadChunk, len(upload.data))]
		result, err := s.workspaceCall(use, runner.WorkspaceRequest{Op: runner.WorkspaceStage, Path: upload.path, Staging: staging, Offset: int64(offset), Data: chunk})
		if err != nil {
			s.discardStaging(use, staging)
			return runner.WorkspaceEntry{}, err
		}
		staging = result.Staging
	}
	result, err := s.workspaceCall(use, runner.WorkspaceRequest{Op: runner.WorkspacePublish, Path: upload.path, Staging: staging, Expected: upload.expected})
	return result.Entry, err
}

func (s *server) discardStaging(use workspaceUse, staging string) {
	if staging == "" {
		return
	}
	cleanup := workspaceUse{ctx: context.WithoutCancel(use.ctx), handle: use.handle, release: use.release}
	if _, err := s.workspaceCall(cleanup, runner.WorkspaceRequest{Op: runner.WorkspaceDiscard, Staging: staging}); err != nil {
		s.log.Warn("rpc: discard staged workspace upload", "staging", staging, "error", err)
	}
}

func (s *server) uploadAbort(p workspaceWriteParams) (jsontext.Value, error) {
	if p.UploadID == "" {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	if err := s.workspace.abort(p.UploadID); err != nil {
		return nil, err
	}
	return jsontext.Value(`{"aborted":true}`), nil
}

func (t *workspaceTable) abort(id string) error {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.init()
	upload := t.uploads[id]
	if upload == nil {
		return nil
	}
	if upload.committing {
		return rpc.NativeKind(rpc.ErrorBusy)
	}
	t.staged -= upload.total
	delete(t.uploads, id)
	return nil
}
