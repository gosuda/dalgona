package rpcserver

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"path"
	"slices"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

const (
	workspaceWatchLimit = 32
	workspaceWatchPaths = 1000
	workspaceWatchBytes = 64 << 10
	workspaceWatchPoll  = 250 * time.Millisecond
)

type workspaceWatch struct {
	handle *workspaceHandle
	cancel context.CancelFunc
}

type workspaceWatchParams struct {
	WorkspaceID string `json:"workspaceId"`
	Path        string `json:"path"`
	Recursive   bool   `json:"recursive"`
}

type workspaceWatching struct {
	WatchID           string `json:"watchId"`
	DirectoryRevision string `json:"directoryRevision"`
}

type workspaceChanged struct {
	WatchID           string   `json:"watchId"`
	DirectoryRevision string   `json:"directoryRevision"`
	Paths             []string `json:"paths"`
	RescanRequired    bool     `json:"rescanRequired"`
}

func (s *server) workspaceWatch(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p workspaceWatchParams
	if err := json.Unmarshal(params, &p); err != nil || p.WorkspaceID == "" || p.Recursive {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	use, err := s.workspaceReader(ctx, p.WorkspaceID, p.Path)
	if err != nil {
		return nil, err
	}
	defer use.release()
	first, err := s.workspaceCall(use, runner.WorkspaceRequest{Op: runner.WorkspaceList, Path: p.Path})
	if err != nil {
		return nil, err
	}
	watchCtx, cancel := context.WithCancel(s.ctx)
	id, err := s.workspace.addWatch(&workspaceWatch{handle: use.handle, cancel: cancel})
	if err != nil {
		cancel()
		return nil, err
	}
	go s.pollWatch(watchCtx, id, use.handle, p.Path, first)
	return json.Marshal(workspaceWatching{WatchID: id, DirectoryRevision: first.Revision})
}

func (t *workspaceTable) addWatch(w *workspaceWatch) (string, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.init()
	if w.handle.closed {
		return "", rpc.NativeKind(rpc.ErrorExpired)
	}
	if len(t.watches) >= workspaceWatchLimit {
		return "", rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	id := newHandle()
	t.watches[id] = w
	return id, nil
}

func (t *workspaceTable) removeWatch(id string) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.init()
	if w := t.watches[id]; w != nil {
		w.cancel()
		delete(t.watches, id)
	}
}

func (s *server) workspaceUnwatch(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p struct {
		WatchID string `json:"watchId"`
	}
	if err := json.Unmarshal(params, &p); err != nil || p.WatchID == "" {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	s.workspace.removeWatch(p.WatchID)
	return jsontext.Value(`{"removed":true}`), nil
}

func (s *server) pollWatch(ctx context.Context, id string, h *workspaceHandle, rel string, last runner.WorkspaceResult) {
	ticker := time.NewTicker(workspaceWatchPoll)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
		if s.workspaceBound(ctx, h) != nil {
			continue
		}
		current, err := h.files.Workspace(ctx, runner.WorkspaceRequest{Op: runner.WorkspaceList, Path: rel})
		if ctx.Err() != nil {
			return
		}
		if err != nil {
			current = runner.WorkspaceResult{Revision: runner.WorkspaceAbsent}
		}
		if current.Revision != last.Revision {
			s.notifyChanged(id, current, changedPaths(rel, last.Entries, current.Entries))
			last = current
		}
	}
}

func (s *server) notifyChanged(id string, current runner.WorkspaceResult, paths []string) {
	note := workspaceChanged{WatchID: id, DirectoryRevision: current.Revision, Paths: paths}
	if current.Revision == runner.WorkspaceAbsent || !withinWatchBudget(paths) {
		note.Paths, note.RescanRequired = []string{}, true
	}
	if err := s.conn.Notify(methodWorkspaceChanged, note); err != nil {
		s.log.Debug("rpc: drop workspace change", "watch", id, "error", err)
	}
}

func withinWatchBudget(paths []string) bool {
	if len(paths) > workspaceWatchPaths {
		return false
	}
	size := 0
	for _, p := range paths {
		size += len(p) + 4
	}
	return size <= workspaceWatchBytes
}

func changedPaths(rel string, before, after []runner.WorkspaceEntry) []string {
	revisions := make(map[string]string, len(before))
	for _, entry := range before {
		revisions[entry.Name] = entry.Revision
	}
	changed := []string{}
	for _, entry := range after {
		previous, existed := revisions[entry.Name]
		delete(revisions, entry.Name)
		if !existed || previous != entry.Revision {
			changed = append(changed, path.Join(rel, entry.Name))
		}
	}
	for name := range revisions {
		changed = append(changed, path.Join(rel, name))
	}
	slices.Sort(changed)
	return changed
}
