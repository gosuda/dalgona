package rpcserver

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"strings"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

const (
	methodWorkspacePermission = "dalgona/workspace/requestPermission"
	methodWorkspaceChanged    = "dalgona/workspace/changed"
	workspaceIdle             = 15 * time.Minute
	workspacePermissionTTL    = 5 * time.Minute
	workspaceDecisionDeadline = workspacePermissionTTL + PageDeadline
	workspaceCursorTTL        = 2 * time.Minute
	workspaceCursorLimit      = 64
	workspacePathBytes        = 4096
	workspaceAllowOnce        = "allowOnce"
)

type workspaceRight uint8

const (
	rightRead workspaceRight = 1 << iota
	rightWrite
	rightTerminal
)

var workspaceRightNames = []struct {
	name  string
	right workspaceRight
}{{"read", rightRead}, {"write", rightWrite}, {"terminal", rightTerminal}}

func workspaceMethods() map[string]method {
	return map[string]method{
		"dalgona/workspace/open":    deadline(workspaceDecisionDeadline, withOperation("dalgona/workspace/open", (*server).workspaceOpen)),
		"dalgona/workspace/close":   deadline(MetadataDeadline, (*server).workspaceClose),
		"dalgona/workspace/list":    deadline(PageDeadline, (*server).workspaceList),
		"dalgona/workspace/search":  deadline(PageDeadline, (*server).workspaceSearch),
		"dalgona/workspace/stat":    deadline(MetadataDeadline, (*server).workspaceStat),
		"dalgona/workspace/read":    deadline(PageDeadline, (*server).workspaceRead),
		"dalgona/workspace/write":   deadline(workspaceDecisionDeadline, (*server).workspaceWrite),
		"dalgona/workspace/mkdir":   deadline(PageDeadline, withOperation("dalgona/workspace/mkdir", (*server).workspaceMkdir)),
		"dalgona/workspace/rename":  deadline(PageDeadline, withOperation("dalgona/workspace/rename", (*server).workspaceRename)),
		"dalgona/workspace/remove":  deadline(workspaceDecisionDeadline, withOperation("dalgona/workspace/remove", (*server).workspaceRemove)),
		"dalgona/workspace/watch":   deadline(PageDeadline, (*server).workspaceWatch),
		"dalgona/workspace/unwatch": deadline(MetadataDeadline, (*server).workspaceUnwatch),
		methodWorkspaceExec:         deadline(workspaceExecDeadline, withOperation(methodWorkspaceExec, (*server).workspaceExec)),
	}
}

type workspaceHandle struct {
	id, threadID, runnerID, binding, cwd, root, pathStyle string
	remote, bound                                         bool
	run                                                   runner.Runner
	files                                                 runner.Workspace
	rights                                                workspaceRight
	used                                                  time.Time
	effects                                               int
	reads                                                 map[uint64]context.CancelFunc
	closed                                                bool
}

type workspaceTable struct {
	mu       sync.Mutex
	handles  map[string]*workspaceHandle
	watches  map[string]*workspaceWatch
	cursors  map[string]*workspaceCursor
	uploads  map[string]*workspaceUpload
	staged   int64
	commands int
	readSeq  uint64
}

type workspaceUse struct {
	ctx     context.Context
	handle  *workspaceHandle
	release func()
}

func (t *workspaceTable) init() {
	if t.handles == nil {
		t.handles = map[string]*workspaceHandle{}
		t.watches = map[string]*workspaceWatch{}
		t.cursors = map[string]*workspaceCursor{}
		t.uploads = map[string]*workspaceUpload{}
	}
}

func (t *workspaceTable) live(id string, now time.Time) *workspaceHandle {
	t.init()
	h := t.handles[id]
	if h == nil || h.closed {
		return nil
	}
	if now.Sub(h.used) < workspaceIdle {
		return h
	}
	t.releaseLocked(h)
	return nil
}

func (t *workspaceTable) acquire(ctx context.Context, id string, right workspaceRight, effect bool) (workspaceUse, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	h := t.live(id, time.Now())
	if h == nil {
		return workspaceUse{}, rpc.NativeKind(rpc.ErrorExpired)
	}
	if h.rights&right != right {
		return workspaceUse{}, rpc.NativeKind(rpc.ErrorPermissionDenied)
	}
	h.used = time.Now()
	if effect {
		h.effects++
		return workspaceUse{ctx: ctx, handle: h, release: sync.OnceFunc(func() { t.settle(h) })}, nil
	}
	ctx, cancel := context.WithCancel(ctx)
	t.readSeq++
	key := t.readSeq
	h.reads[key] = cancel
	return workspaceUse{ctx: ctx, handle: h, release: sync.OnceFunc(func() { t.forgetRead(h, key) })}, nil
}

func (t *workspaceTable) settle(h *workspaceHandle) {
	t.mu.Lock()
	defer t.mu.Unlock()
	h.effects--
	if h.closed && h.effects == 0 {
		t.releaseLocked(h)
	}
}

func (t *workspaceTable) forgetRead(h *workspaceHandle, key uint64) {
	t.mu.Lock()
	cancel := h.reads[key]
	delete(h.reads, key)
	t.mu.Unlock()
	if cancel != nil {
		cancel()
	}
}

func (t *workspaceTable) touch(id string) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if h := t.live(id, time.Now()); h != nil {
		h.used = time.Now()
	}
}

func (t *workspaceTable) revoke(h *workspaceHandle) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if h.effects > 0 {
		h.closed = true
		t.cancelDependents(h)
		return
	}
	t.releaseLocked(h)
}

func (t *workspaceTable) close(id string) (string, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.init()
	h := t.handles[id]
	if h == nil || h.closed {
		return "notFound", nil
	}
	if h.effects > 0 {
		return "", rpc.NativeKind(rpc.ErrorBusy)
	}
	t.releaseLocked(h)
	return "closed", nil
}

func closeWorkspaceFiles(files runner.Workspace) {
	if closer, ok := files.(interface{ Close() error }); ok {
		_ = closer.Close()
	}
}

func (t *workspaceTable) releaseLocked(h *workspaceHandle) {
	h.closed = true
	t.cancelDependents(h)
	if h.effects == 0 && t.handles[h.id] == h {
		delete(t.handles, h.id)
		closeWorkspaceFiles(h.files)
	}
}

func (t *workspaceTable) closeAll() {
	t.mu.Lock()
	handles := make([]*workspaceHandle, 0, len(t.handles))
	for id, h := range t.handles {
		h.closed = true
		t.cancelDependents(h)
		delete(t.handles, id)
		handles = append(handles, h)
	}
	t.mu.Unlock()
	for _, h := range handles {
		closeWorkspaceFiles(h.files)
	}
}

func (t *workspaceTable) cancelDependents(h *workspaceHandle) {
	for key, cancel := range h.reads {
		cancel()
		delete(h.reads, key)
	}
	for id, w := range t.watches {
		if w.handle == h {
			w.cancel()
			delete(t.watches, id)
		}
	}
	for id, c := range t.cursors {
		if c.handle == h {
			delete(t.cursors, id)
		}
	}
	for id, u := range t.uploads {
		if u.handle == h && !u.committing {
			t.staged -= u.total
			delete(t.uploads, id)
		}
	}
}

func (t *workspaceTable) admit(h *workspaceHandle) error {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.fullLocked() {
		return rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	t.handles[h.id] = h
	return nil
}

func (t *workspaceTable) full() bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.fullLocked()
}

func (t *workspaceTable) fullLocked() bool {
	t.init()
	now := time.Now()
	live := 0
	for id := range t.handles {
		if t.live(id, now) != nil {
			live++
		}
	}
	return live >= connectionWorkspaces
}

func (s *server) workspaceClose(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p struct {
		WorkspaceID string `json:"workspaceId"`
	}
	if err := json.Unmarshal(params, &p); err != nil || p.WorkspaceID == "" {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	status, err := s.workspace.close(p.WorkspaceID)
	if err != nil {
		return nil, err
	}
	return json.Marshal(struct {
		Status string `json:"status"`
	}{Status: status})
}

func (s *server) workspaceUse(ctx context.Context, id string, right workspaceRight, effect bool) (workspaceUse, error) {
	use, err := s.workspace.acquire(ctx, id, right, effect)
	if err != nil {
		return workspaceUse{}, err
	}
	if err := s.workspaceBound(use.ctx, use.handle); err != nil {
		use.release()
		return workspaceUse{}, err
	}
	return use, nil
}

func (s *server) workspaceBound(ctx context.Context, h *workspaceHandle) error {
	thread, err := s.host.Resume(ctx, h.threadID)
	if err != nil {
		return workspaceFailure(err, false)
	}
	binding, _, err := thread.BindingRevision()
	if err != nil {
		return rpc.NativeKind(rpc.ErrorUnavailable)
	}
	if thread.Runner() == h.run && binding == h.binding {
		return nil
	}
	s.workspace.revoke(h)
	return rpc.NativeKind(rpc.ErrorExpired)
}

type workspaceGrant struct {
	threadID, runnerID, bindingRevision, root string
	remote                                    bool
	run                                       runner.Runner
	handle                                    *workspaceHandle
	validate                                  func() error
}

func (s *server) workspaceTerminal(id string) (workspaceGrant, func(), error) {
	use, err := s.workspaceUse(s.ctx, id, rightTerminal, true)
	if err != nil {
		return workspaceGrant{}, nil, err
	}
	h := use.handle
	grant := workspaceGrant{threadID: h.threadID, runnerID: h.runnerID, bindingRevision: h.binding, root: h.root, remote: h.remote, run: h.run, handle: h}
	grant.validate = func() error { return s.validateTerminalWorkspace(h) }
	return grant, use.release, nil
}

func (s *server) validateTerminalWorkspace(h *workspaceHandle) error {
	s.workspace.mu.Lock()
	current := s.workspace.live(h.id, time.Now())
	s.workspace.mu.Unlock()
	if current != h {
		return rpc.NativeKind(rpc.ErrorExpired)
	}
	return s.workspaceBound(s.ctx, h)
}

func (s *server) workspaceTouch(id string) { s.workspace.touch(id) }

type workspaceOpenParams struct {
	ThreadID string   `json:"threadId"`
	Root     string   `json:"root"`
	Rights   []string `json:"rights"`
}

type workspaceOpened struct {
	WorkspaceID     string   `json:"workspaceId"`
	ThreadID        string   `json:"threadId"`
	RunnerID        string   `json:"runnerId"`
	BindingRevision string   `json:"bindingRevision"`
	Root            string   `json:"root"`
	PathStyle       string   `json:"pathStyle"`
	Rights          []string `json:"rights"`
	ExpiresAt       string   `json:"expiresAt"`
}

type workspacePermission struct {
	ThreadID        string   `json:"threadId"`
	RunnerID        string   `json:"runnerId"`
	BindingRevision string   `json:"bindingRevision"`
	Root            string   `json:"root"`
	Rights          []string `json:"rights"`
	Reason          string   `json:"reason"`
}

func parseRights(names []string) (workspaceRight, error) {
	var rights workspaceRight
	for _, name := range names {
		right := rightNamed(name)
		if right == 0 || rights&right != 0 {
			return 0, rpc.NativeKind(rpc.ErrorInvalidParams)
		}
		rights |= right
	}
	if rights == 0 {
		return 0, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return rights, nil
}

func rightNamed(name string) workspaceRight {
	for _, known := range workspaceRightNames {
		if known.name == name {
			return known.right
		}
	}
	return 0
}

func (r workspaceRight) names() []string {
	names := make([]string, 0, len(workspaceRightNames))
	for _, known := range workspaceRightNames {
		if r&known.right != 0 {
			names = append(names, known.name)
		}
	}
	return names
}

func (s *server) workspaceOpen(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p workspaceOpenParams
	if err := json.Unmarshal(params, &p); err != nil || p.Root != "cwd" || p.ThreadID == "" {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	rights, err := parseRights(p.Rights)
	if err != nil {
		return nil, err
	}
	h, err := s.workspaceCandidate(ctx, p.ThreadID, rights)
	if err != nil {
		return nil, err
	}
	if err := s.authorizeWorkspace(ctx, h); err != nil {
		s.closeCandidate(h)
		return nil, err
	}
	if err := s.bindWorkspace(ctx, h); err != nil {
		s.closeCandidate(h)
		return nil, err
	}
	if err := s.workspace.admit(h); err != nil {
		s.closeCandidate(h)
		return nil, err
	}
	return json.Marshal(workspaceOpened{
		WorkspaceID: h.id, ThreadID: h.threadID, RunnerID: h.runnerID, BindingRevision: h.binding, Root: h.root,
		PathStyle: h.pathStyle, Rights: h.rights.names(), ExpiresAt: rpc.FormatTimestamp(h.used.Add(workspaceIdle)),
	})
}

func (s *server) closeCandidate(h *workspaceHandle) {
	closeWorkspaceFiles(h.files)
}

func (s *server) workspaceCandidate(ctx context.Context, threadID string, rights workspaceRight) (*workspaceHandle, error) {
	if _, ok := s.info(threadID); !ok {
		return nil, rpc.NativeKind(rpc.ErrorNotFound)
	}
	thread, err := s.host.Resume(ctx, threadID)
	if err != nil {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	if s.workspace.full() {
		return nil, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	binding, bound, err := thread.BindingRevision()
	if err != nil {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	session, err := openPinnedSession(ctx, thread.Runner(), thread.Cwd())
	if err != nil {
		return nil, err
	}
	h := &workspaceHandle{
		id: newHandle(), threadID: threadID, runnerID: thread.RunnerName(), binding: binding, cwd: thread.Cwd(),
		run: thread.Runner(), files: session, rights: rights, reads: map[uint64]context.CancelFunc{}, pathStyle: pathStyle(),
	}
	if remote, ok := h.run.(interface{ RemoteHost() bool }); ok && remote.RemoteHost() {
		h.remote, h.pathStyle = true, "posix"
	}
	h.root, h.bound, h.used = session.DisplayRoot(), bound, time.Now()
	return h, nil
}

func openPinnedSession(ctx context.Context, run runner.Runner, cwd string) (runner.WorkspaceHandle, error) {
	opener, ok := run.(runner.WorkspaceOpener)
	if !ok {
		return nil, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorIncompatible, Stage: "runner"})
	}
	session, err := opener.OpenWorkspace(ctx, cwd)
	if err != nil {
		if errors.Is(err, runner.ErrWorkspaceUnsupported) {
			return nil, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorIncompatible, Stage: "runner"})
		}
		return nil, workspaceFailure(err, false)
	}
	return session, nil
}

func (s *server) authorizeWorkspace(ctx context.Context, h *workspaceHandle) error {
	return s.askWorkspace(ctx, workspacePermission{
		ThreadID: h.threadID, RunnerID: h.runnerID, BindingRevision: h.binding, Root: h.root,
		Rights: h.rights.names(), Reason: openReason(h),
	})
}

func openReason(h *workspaceHandle) string {
	parts := make([]string, 0, len(workspaceRightNames))
	if h.rights&rightRead != 0 {
		parts = append(parts, "read raw file contents")
	}
	if h.rights&rightWrite != 0 {
		parts = append(parts, "change files")
	}
	if h.rights&rightTerminal != 0 {
		parts = append(parts, "run commands")
	}
	return "Allow this window to " + strings.Join(parts, ", ") + " in " + h.root + " on runner " + h.runnerID + "."
}

func (s *server) askWorkspace(ctx context.Context, p workspacePermission) error {
	askCtx, cancel := context.WithTimeout(ctx, workspacePermissionTTL)
	defer cancel()
	requestID := "workspace-" + newHandle()
	if !s.registerWorkspaceDecision(requestID, p.ThreadID, cancel) {
		return rpc.NativeKind(rpc.ErrorCancelled)
	}
	var response struct {
		Decision string `json:"decision"`
	}
	err := s.conn.CallID(askCtx, requestID, methodWorkspacePermission, p, &response)
	accepted := s.claimWorkspaceDecision(askCtx, requestID)
	resolution := "interrupted"
	if accepted && err == nil {
		resolution = "declined"
		if response.Decision == workspaceAllowOnce {
			resolution = "answered"
		}
	}
	s.resolved(p.ThreadID, requestID, resolution)
	switch {
	case resolution == "answered":
		return nil
	case ctx.Err() != nil:
		return rpc.NativeKind(rpc.ErrorCancelled)
	}
	return rpc.NativeKind(rpc.ErrorPermissionDenied)
}

func (s *server) registerWorkspaceDecision(requestID, threadID string, cancel context.CancelFunc) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closing {
		return false
	}
	s.pending[requestID] = cancel
	s.threads[requestID] = threadID
	return true
}

func (s *server) claimWorkspaceDecision(ctx context.Context, requestID string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.takeRequestLocked(requestID) != nil && ctx.Err() == nil
}

func (s *server) bindWorkspace(ctx context.Context, h *workspaceHandle) error {
	thread, err := s.host.Resume(ctx, h.threadID)
	if err != nil || thread.Runner() != h.run {
		return rpc.NativeKind(rpc.ErrorExpired)
	}
	binding, bound, err := thread.BindingRevision()
	if err != nil {
		return rpc.NativeKind(rpc.ErrorUnavailable)
	}
	if binding != h.binding && h.bound {
		return rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorStaleRevision, CurrentRevision: binding})
	}
	h.binding, h.bound, h.used = binding, bound, time.Now()
	return nil
}
