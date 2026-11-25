package rpcserver

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"io/fs"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

const (
	workspaceListDefault = 100
	workspaceListMax     = 500
	workspacePageBytes   = 1 << 20
	workspaceReadDefault = 64 << 10
	workspaceReadMax     = 1 << 20
)

type workspaceMetadata struct {
	Name       string `json:"name"`
	Kind       string `json:"kind"`
	Size       string `json:"size"`
	ModifiedAt string `json:"modifiedAt"`
	Revision   string `json:"revision"`
	Readable   bool   `json:"readable"`
	Writable   bool   `json:"writable"`
}

func metadataOf(entry runner.WorkspaceEntry) workspaceMetadata {
	return workspaceMetadata{
		Name: entry.Name, Kind: entry.Kind, Size: rpc.FormatDecimal(uint64(max(entry.Size, 0))),
		ModifiedAt: rpc.FormatTimestamp(entry.ModTime), Revision: entry.Revision, Readable: entry.Readable, Writable: entry.Writable,
	}
}

var workspaceKinds = []struct {
	err  error
	kind rpc.ErrorKind
}{
	{fs.ErrNotExist, rpc.ErrorNotFound},
	{runner.ErrWorkspaceEscape, rpc.ErrorPermissionDenied},
	{runner.ErrWorkspaceKind, rpc.ErrorInvalidParams},
	{runner.ErrWorkspaceExists, rpc.ErrorStaleRevision},
	{runner.ErrWorkspaceRoot, rpc.ErrorInvalidParams},
	{runner.ErrWorkspaceLimit, rpc.ErrorLimitExceeded},
	{runner.ErrWorkspaceNotEmpty, rpc.ErrorInvalidParams},
	{runner.ErrWorkspaceOffset, rpc.ErrorInvalidParams},
	{runner.ErrWorkspaceRequest, rpc.ErrorInvalidParams},
}

func workspaceFailure(err error, mutates bool) error {
	if stale, ok := errors.AsType[*runner.StaleRevisionError](err); ok {
		return rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorStaleRevision, CurrentRevision: stale.Current})
	}
	if _, native := rpc.ErrorDataOf(err); native {
		return err
	}
	for _, known := range workspaceKinds {
		if errors.Is(err, known.err) {
			return rpc.NativeKind(known.kind)
		}
	}
	switch {
	case mutates:
		return rpc.NativeKind(rpc.ErrorUnknownOutcome)
	case errors.Is(err, context.Canceled), errors.Is(err, context.DeadlineExceeded):
		return rpc.NativeKind(rpc.ErrorCancelled)
	}
	return rpc.NativeKind(rpc.ErrorUnavailable)
}

func checkWorkspacePath(rel string, rootAllowed bool, style string) error {
	if rel == "" {
		return checkRootAllowed(rootAllowed)
	}
	if !validWorkspaceName(rel, style) {
		return rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return checkWorkspaceParts(rel)
}

func checkRootAllowed(rootAllowed bool) error {
	if rootAllowed {
		return nil
	}
	return rpc.NativeKind(rpc.ErrorInvalidParams)
}

func validWorkspaceName(rel, style string) bool {
	if len(rel) > workspacePathBytes || !utf8.ValidString(rel) || strings.IndexByte(rel, 0) >= 0 || hasDrivePrefix(rel) {
		return false
	}
	return style != "windows" || !strings.ContainsRune(rel, '\\')
}

func checkWorkspaceParts(rel string) error {
	for part := range strings.SplitSeq(rel, "/") {
		if part == "" || part == "." || part == ".." {
			return rpc.NativeKind(rpc.ErrorInvalidParams)
		}
	}
	return nil
}

func hasDrivePrefix(rel string) bool {
	return len(rel) >= 2 && rel[1] == ':' && (rel[0]|0x20 >= 'a' && rel[0]|0x20 <= 'z')
}

func (s *server) workspaceCall(use workspaceUse, req runner.WorkspaceRequest) (runner.WorkspaceResult, error) {
	req.Root = ""
	req.Token = ""
	result, err := use.handle.files.Workspace(use.ctx, req)
	if err != nil {
		return result, workspaceFailure(err, !req.Op.Reads())
	}
	return result, nil
}

type workspacePathParams struct {
	WorkspaceID string `json:"workspaceId"`
	Path        string `json:"path"`
}

func (s *server) workspaceStat(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p workspacePathParams
	if err := json.Unmarshal(params, &p); err != nil || p.WorkspaceID == "" {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	use, err := s.workspaceReader(ctx, p.WorkspaceID, p.Path)
	if err != nil {
		return nil, err
	}
	defer use.release()
	result, err := s.workspaceCall(use, runner.WorkspaceRequest{Op: runner.WorkspaceStat, Path: p.Path})
	if err != nil {
		return nil, err
	}
	return json.Marshal(metadataOf(result.Entry))
}

func (s *server) workspaceReader(ctx context.Context, id, rel string) (workspaceUse, error) {
	use, err := s.workspaceUse(ctx, id, rightRead, false)
	if err != nil {
		return workspaceUse{}, err
	}
	if err := checkWorkspacePath(rel, true, use.handle.pathStyle); err != nil {
		use.release()
		return workspaceUse{}, err
	}
	return use, nil
}

type workspaceCursorKind uint8

const (
	listingCursor workspaceCursorKind = iota
	searchCursor
)

type workspaceCursor struct {
	handle     *workspaceHandle
	kind       workspaceCursorKind
	path       string
	revision   string
	offset     int
	expires    time.Time
	matches    []workspaceMatch
	incomplete bool
	searchCut  workspaceSearchCut
}

type workspaceListParams struct {
	WorkspaceID string `json:"workspaceId"`
	Path        string `json:"path"`
	Cursor      string `json:"cursor"`
	Limit       int    `json:"limit"`
}

type workspaceListing struct {
	Entries           []workspaceMetadata `json:"entries"`
	NextCursor        string              `json:"nextCursor,omitzero"`
	DirectoryRevision string              `json:"directoryRevision"`
}

func pageLimit(limit, fallback, ceiling int) (int, error) {
	if limit == 0 {
		return fallback, nil
	}
	if limit < 0 || limit > ceiling {
		return 0, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return limit, nil
}

func (s *server) workspaceList(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p workspaceListParams
	if err := json.Unmarshal(params, &p); err != nil || p.WorkspaceID == "" {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	limit, err := pageLimit(p.Limit, workspaceListDefault, workspaceListMax)
	if err != nil {
		return nil, err
	}
	use, err := s.workspaceReader(ctx, p.WorkspaceID, p.Path)
	if err != nil {
		return nil, err
	}
	defer use.release()
	offset, err := s.workspace.resume(p.Cursor, use.handle, p.Path)
	if err != nil {
		return nil, err
	}
	result, err := s.workspaceCall(use, runner.WorkspaceRequest{Op: runner.WorkspaceList, Path: p.Path})
	if err != nil {
		return nil, err
	}
	return s.listingPage(use.handle, p, result, offset, limit)
}

func fillListingPage(page *workspaceListing, entries []runner.WorkspaceEntry, offset, limit int) (int, error) {
	size := 0
	end := offset
	for end < len(entries) && len(page.Entries) < limit {
		metadata := metadataOf(entries[end])
		encoded, err := json.Marshal(metadata)
		if err != nil {
			return 0, internalError(err)
		}
		if size+len(encoded) > workspacePageBytes-rpc.IDLimit*4 && len(page.Entries) > 0 {
			break
		}
		size += len(encoded) + 1
		page.Entries = append(page.Entries, metadata)
		end++
	}
	return end, nil
}

func (s *server) listingPage(h *workspaceHandle, p workspaceListParams, result runner.WorkspaceResult, offset, limit int) (jsontext.Value, error) {
	if p.Cursor != "" && !s.workspace.sameRevision(p.Cursor, result.Revision) {
		return nil, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorStaleRevision, CurrentRevision: result.Revision})
	}
	page := workspaceListing{Entries: []workspaceMetadata{}, DirectoryRevision: result.Revision}
	end, err := fillListingPage(&page, result.Entries, offset, limit)
	if err != nil {
		return nil, err
	}
	if end < len(result.Entries) {
		cursor, err := s.workspace.remember(&workspaceCursor{handle: h, path: p.Path, revision: result.Revision, offset: end})
		if err != nil {
			return nil, err
		}
		page.NextCursor = cursor
	}
	s.workspace.forgetCursor(p.Cursor)
	return json.Marshal(page)
}

func (t *workspaceTable) remember(c *workspaceCursor) (string, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.init()
	now := time.Now()
	for id, existing := range t.cursors {
		if now.After(existing.expires) {
			delete(t.cursors, id)
		}
	}
	if len(t.cursors) >= workspaceCursorLimit {
		return "", rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	c.expires = now.Add(workspaceCursorTTL)
	id := newHandle()
	t.cursors[id] = c
	return id, nil
}

func (t *workspaceTable) cursor(id string, h *workspaceHandle, rel string, kind workspaceCursorKind) (*workspaceCursor, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.init()
	c := t.cursors[id]
	if c == nil || c.handle != h || c.kind != kind || c.path != rel || time.Now().After(c.expires) {
		return nil, rpc.NativeKind(rpc.ErrorExpired)
	}
	return c, nil
}

func (t *workspaceTable) resume(id string, h *workspaceHandle, rel string) (int, error) {
	if id == "" {
		return 0, nil
	}
	c, err := t.cursor(id, h, rel, listingCursor)
	if err != nil {
		return 0, err
	}
	return c.offset, nil
}

func (t *workspaceTable) sameRevision(id, revision string) bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	c := t.cursors[id]
	return c != nil && c.revision == revision
}

func (t *workspaceTable) forgetCursor(id string) {
	if id == "" {
		return
	}
	t.mu.Lock()
	delete(t.cursors, id)
	t.mu.Unlock()
}

type workspaceReadParams struct {
	WorkspaceID      string `json:"workspaceId"`
	Path             string `json:"path"`
	ExpectedRevision string `json:"expectedRevision"`
	Offset           string `json:"offset"`
	Limit            int64  `json:"limit"`
}

type workspaceChunk struct {
	Revision   string `json:"revision"`
	Total      string `json:"total"`
	Offset     string `json:"offset"`
	Next       string `json:"next"`
	DataBase64 []byte `json:"dataBase64"`
}

func (p workspaceReadParams) window() (int64, int64, error) {
	var offset uint64
	if p.Offset != "" {
		parsed, err := rpc.ParseDecimal(p.Offset)
		if err != nil || parsed > 1<<62 {
			return 0, 0, rpc.NativeKind(rpc.ErrorInvalidParams)
		}
		offset = parsed
	}
	limit := p.Limit
	if limit == 0 {
		limit = workspaceReadDefault
	}
	if limit < 0 || limit > workspaceReadMax {
		return 0, 0, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return int64(offset), limit, nil
}

func (s *server) workspaceRead(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p workspaceReadParams
	if err := json.Unmarshal(params, &p); err != nil || p.WorkspaceID == "" || len(p.ExpectedRevision) > rpc.IDLimit {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	offset, limit, err := p.window()
	if err != nil {
		return nil, err
	}
	use, err := s.workspaceReader(ctx, p.WorkspaceID, p.Path)
	if err != nil {
		return nil, err
	}
	defer use.release()
	result, err := s.workspaceCall(use, runner.WorkspaceRequest{Op: runner.WorkspaceRead, Path: p.Path, Expected: p.ExpectedRevision, Offset: offset, Limit: limit})
	if err != nil {
		return nil, err
	}
	return readChunk(result, offset)
}

func readChunk(result runner.WorkspaceResult, offset int64) (jsontext.Value, error) {
	if offset > result.Total {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	next := offset + int64(len(result.Data))
	if result.Total < 0 || offset < 0 || next < 0 {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return json.Marshal(workspaceChunk{
		Revision: result.Revision, Total: rpc.FormatDecimal(uint64(result.Total)), Offset: rpc.FormatDecimal(uint64(offset)),
		Next: rpc.FormatDecimal(uint64(next)), DataBase64: result.Data,
	})
}

func (s *server) workspaceWriter(ctx context.Context, id, rel string) (workspaceUse, error) {
	use, err := s.workspaceUse(ctx, id, rightWrite, true)
	if err != nil {
		return workspaceUse{}, err
	}
	if err := checkWorkspacePath(rel, false, use.handle.pathStyle); err != nil {
		use.release()
		return workspaceUse{}, err
	}
	return use, nil
}

type workspaceMkdirParams struct {
	WorkspaceID string `json:"workspaceId"`
	Path        string `json:"path"`
	Recursive   *bool  `json:"recursive"`
}

type workspaceMetadataResult struct {
	Metadata workspaceMetadata `json:"metadata"`
}

func (s *server) workspaceMkdir(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p workspaceMkdirParams
	if err := json.Unmarshal(params, &p); err != nil || p.WorkspaceID == "" || p.Recursive == nil {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	use, err := s.workspaceWriter(ctx, p.WorkspaceID, p.Path)
	if err != nil {
		return nil, err
	}
	defer use.release()
	result, err := s.workspaceCall(use, runner.WorkspaceRequest{Op: runner.WorkspaceMkdir, Path: p.Path, Recursive: *p.Recursive})
	if err != nil {
		return nil, err
	}
	return json.Marshal(workspaceMetadataResult{Metadata: metadataOf(result.Entry)})
}

type workspaceRenameParams struct {
	WorkspaceID      string `json:"workspaceId"`
	From             string `json:"from"`
	To               string `json:"to"`
	ExpectedRevision string `json:"expectedRevision"`
	Replace          *bool  `json:"replace"`
}

func (s *server) workspaceRename(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p workspaceRenameParams
	if err := json.Unmarshal(params, &p); err != nil || p.WorkspaceID == "" || p.Replace == nil || !validRevision(p.ExpectedRevision) {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	use, err := s.workspaceWriter(ctx, p.WorkspaceID, p.From)
	if err != nil {
		return nil, err
	}
	defer use.release()
	if pathErr := checkWorkspacePath(p.To, false, use.handle.pathStyle); pathErr != nil {
		return nil, pathErr
	}
	result, err := s.workspaceCall(use, runner.WorkspaceRequest{
		Op: runner.WorkspaceRename, Path: p.From, To: p.To, Expected: p.ExpectedRevision, Replace: *p.Replace,
	})
	if err != nil {
		return nil, err
	}
	return json.Marshal(workspaceMetadataResult{Metadata: metadataOf(result.Entry)})
}

func validRevision(revision string) bool { return revision != "" && len(revision) <= rpc.IDLimit }

type workspaceRemoveParams struct {
	WorkspaceID      string `json:"workspaceId"`
	Path             string `json:"path"`
	ExpectedRevision string `json:"expectedRevision"`
	Recursive        *bool  `json:"recursive"`
}

func (s *server) workspaceRemove(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p workspaceRemoveParams
	if err := json.Unmarshal(params, &p); err != nil || p.WorkspaceID == "" || p.Recursive == nil || !validRevision(p.ExpectedRevision) {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	use, err := s.workspaceWriter(ctx, p.WorkspaceID, p.Path)
	if err != nil {
		return nil, err
	}
	defer use.release()
	if *p.Recursive {
		if err := s.askWorkspace(use.ctx, removalPermission(use.handle, p.Path)); err != nil {
			return nil, err
		}
	}
	if _, err := s.workspaceCall(use, runner.WorkspaceRequest{
		Op: runner.WorkspaceRemove, Path: p.Path, Expected: p.ExpectedRevision, Recursive: *p.Recursive,
	}); err != nil {
		return nil, err
	}
	return jsontext.Value(`{"removed":true}`), nil
}

func removalPermission(h *workspaceHandle, rel string) workspacePermission {
	return workspacePermission{
		ThreadID: h.threadID, RunnerID: h.runnerID, BindingRevision: h.binding, Root: h.root, Rights: []string{"write"},
		Reason: "Delete " + rel + " and everything inside it from " + h.root + " on runner " + h.runnerID + ". This cannot be undone.",
	}
}
