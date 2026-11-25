package rpcserver

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

type workspaceWindow struct {
	t         *testing.T
	conn      *rpc.Conn
	decisions chan string
	asked     chan jsontext.Value
	changes   chan workspaceChanged
}

type workspaceClosed struct {
	Status string `json:"status"`
}

func (w *workspaceWindow) Notification(method string, params jsontext.Value) {
	if method != methodWorkspaceChanged {
		return
	}
	if err := validateDalgona(w.t, "dalgona.workspace.changed.json", params); err != nil {
		w.t.Errorf("change notification %s: %v", params, err)
	}
	var note workspaceChanged
	if err := json.Unmarshal(params, &note); err != nil {
		w.t.Errorf("decode change %s: %v", params, err)
	}
	w.changes <- note
}

func (w *workspaceWindow) Request(ctx context.Context, method string, params jsontext.Value) (any, error) {
	if method != methodWorkspacePermission {
		return nil, errors.New("unexpected server request " + method)
	}
	if err := validateDalgona(w.t, "dalgona.workspace.requestPermission.params.json", params); err != nil {
		w.t.Errorf("permission request %s: %v", params, err)
	}
	w.asked <- params
	select {
	case decision := <-w.decisions:
		return map[string]string{"decision": decision}, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

func openWorkspaceWindow(t *testing.T, r *rig) *workspaceWindow {
	t.Helper()
	serverEnd, clientEnd := net.Pipe()
	w := &workspaceWindow{t: t, decisions: make(chan string, 64), asked: make(chan jsontext.Value, 64), changes: make(chan workspaceChanged, 256)}
	served := make(chan error, 1)
	host := r.host
	go func() { served <- ServeWith(t.Context(), serverEnd, host, ServeOptions{}) }()
	w.conn = rpc.Dial(t.Context(), clientEnd, w)
	t.Cleanup(func() {
		_ = w.conn.Close()
		select {
		case serveErr := <-served:
			_ = serveErr
		case <-time.After(waitLimit):
			t.Error("server did not stop after the window closed")
		}
	})
	return w
}

func (w *workspaceWindow) call(method string, params, out any) error {
	w.t.Helper()
	var raw jsontext.Value
	if err := w.conn.Call(w.t.Context(), method, params, &raw); err != nil {
		return err
	}
	if err := validateDalgona(w.t, dalgonaSchemaName(method, ".result.json"), raw); err != nil {
		w.t.Errorf("%s result %s: %v", method, raw, err)
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}

func (w *workspaceWindow) must(method string, params, out any) {
	w.t.Helper()
	encoded, err := json.Marshal(params)
	if err != nil {
		w.t.Fatal(err)
	}
	if err := validateDalgona(w.t, dalgonaSchemaName(method, ".params.json"), encoded); err != nil {
		w.t.Fatalf("%s params %s: %v", method, encoded, err)
	}
	if err := w.call(method, params, out); err != nil {
		w.t.Fatalf("%s: %v", method, err)
	}
}

func (w *workspaceWindow) open(threadID string, rights ...string) workspaceOpened {
	w.t.Helper()
	w.decisions <- workspaceAllowOnce
	var opened workspaceOpened
	w.must("dalgona/workspace/open", map[string]any{"threadId": threadID, "root": "cwd", "rights": rights, "operationId": rpc.NewOperationID()}, &opened)
	<-w.asked
	return opened
}

func (w *workspaceWindow) stat(id, rel string) workspaceMetadata {
	w.t.Helper()
	var metadata workspaceMetadata
	w.must("dalgona/workspace/stat", map[string]any{"workspaceId": id, "path": rel}, &metadata)
	return metadata
}

func (w *workspaceWindow) write(id, rel, expected string, data []byte) (workspaceMetadata, error) {
	w.t.Helper()
	sum := sha256.Sum256(data)
	var begun workspaceUploadBegun
	begin := map[string]any{
		"stage": "begin", "workspaceId": id, "path": rel, "expectedRevision": expected,
		"total": rpc.FormatDecimal(uint64(len(data))), "digest": hex.EncodeToString(sum[:]), "operationId": rpc.NewOperationID(),
	}
	if err := w.call("dalgona/workspace/write", begin, &begun); err != nil {
		return workspaceMetadata{}, err
	}
	chunk := map[string]any{"stage": "chunk", "uploadId": begun.UploadID, "offset": "0", "dataBase64": base64.StdEncoding.EncodeToString(data)}
	if err := w.call("dalgona/workspace/write", chunk, nil); err != nil {
		return workspaceMetadata{}, err
	}
	var committed workspaceMetadataResult
	err := w.call("dalgona/workspace/write", map[string]any{"stage": "commit", "uploadId": begun.UploadID, "operationId": rpc.NewOperationID()}, &committed)
	return committed.Metadata, err
}

func nativeKindOf(t *testing.T, err error) rpc.ErrorKind {
	t.Helper()
	data, ok := rpc.ErrorDataOf(err)
	if !ok {
		t.Fatalf("error %v carries no native kind", err)
	}
	return data.Kind
}

func refusedWith(t *testing.T, err error, want rpc.ErrorKind) rpc.ErrorData {
	t.Helper()
	if err == nil {
		t.Fatalf("call succeeded, want %s", want)
	}
	if got := nativeKindOf(t, err); got != want {
		t.Fatalf("error kind %s (%v), want %s", got, err, want)
	}
	data, _ := rpc.ErrorDataOf(err)
	return data
}

func startWorkspaceThread(t *testing.T, r *rig, cwd string) string {
	t.Helper()
	thread, err := r.host.Start(t.Context(), agent.ThreadSpec{Cwd: cwd})
	if err != nil {
		t.Fatal(err)
	}
	return thread.ID()
}

func remoteRunners(r *rig) {
	r.cfg.Runner = func(ctx context.Context, id, _ string) (runner.Runner, error) {
		local, err := runner.NewLocal(filepath.Join(r.home, "sessions", id, "runs"))
		if err != nil {
			return nil, err
		}
		serverEnd, clientEnd := net.Pipe()
		go func() { _ = runner.Serve(r.t.Context(), serverEnd, local) }()
		return runner.NewRemote(ctx, "box-"+id, clientEnd)
	}
	r.openHost()
}

func eachWorkspaceRunner(t *testing.T, run func(t *testing.T, r *rig)) {
	t.Run("local", func(t *testing.T) { run(t, newRig(t, "")) })
	t.Run("remote", func(t *testing.T) {
		r := newRig(t, "")
		remoteRunners(r)
		run(t, r)
	})
}

func TestWorkspaceOpenCloseCyclesLeakNothing(t *testing.T) {
	r := newRig(t, "")
	threadID := startWorkspaceThread(t, r, r.cwd)
	w := openWorkspaceWindow(t, r)
	payload := []byte("x")
	sum := sha256.Sum256(payload)
	for cycle := range connectionWorkspaces + 4 {
		opened := w.open(threadID, "read", "write")
		for range 3 {
			w.must("dalgona/workspace/watch", map[string]any{"workspaceId": opened.WorkspaceID, "path": "", "recursive": false}, nil)
		}
		begin := map[string]any{
			"stage": "begin", "workspaceId": opened.WorkspaceID, "path": "a.txt", "expectedRevision": runner.WorkspaceAbsent,
			"total": "1", "digest": hex.EncodeToString(sum[:]), "operationId": rpc.NewOperationID(),
		}
		w.must("dalgona/workspace/write", begin, nil)
		var closed workspaceClosed
		w.must("dalgona/workspace/close", map[string]any{"workspaceId": opened.WorkspaceID}, &closed)
		if closed.Status != "closed" {
			t.Fatalf("cycle %d: close status %q, want closed", cycle, closed.Status)
		}
		w.must("dalgona/workspace/close", map[string]any{"workspaceId": opened.WorkspaceID}, &closed)
		if closed.Status != "notFound" {
			t.Fatalf("cycle %d: repeated close status %q, want notFound", cycle, closed.Status)
		}
		refusedWith(t, w.call("dalgona/workspace/stat", map[string]any{"workspaceId": opened.WorkspaceID, "path": ""}, nil), rpc.ErrorExpired)
	}
	for range connectionWorkspaces {
		w.open(threadID, "read")
	}
	err := w.call("dalgona/workspace/open", map[string]any{"threadId": threadID, "root": "cwd", "rights": []string{"read"}, "operationId": rpc.NewOperationID()}, nil)
	refusedWith(t, err, rpc.ErrorLimitExceeded)
	if _, statErr := os.Stat(filepath.Join(r.cwd, "a.txt")); !errors.Is(statErr, os.ErrNotExist) {
		t.Fatalf("an aborted upload reached the workspace: %v", statErr)
	}
}

func TestWorkspaceOpenRequiresAllowOnce(t *testing.T) {
	r := newRig(t, "")
	threadID := startWorkspaceThread(t, r, r.cwd)
	w := openWorkspaceWindow(t, r)
	w.decisions <- "decline"
	err := w.call("dalgona/workspace/open", map[string]any{"threadId": threadID, "root": "cwd", "rights": []string{"read", "terminal"}, "operationId": rpc.NewOperationID()}, nil)
	refusedWith(t, err, rpc.ErrorPermissionDenied)
	var asked workspacePermission
	if decodeErr := json.Unmarshal(<-w.asked, &asked); decodeErr != nil {
		t.Fatal(decodeErr)
	}
	if asked.ThreadID != threadID || asked.Root != r.cwd || !slices.Equal(asked.Rights, []string{"read", "terminal"}) {
		t.Fatalf("permission request %+v does not name the thread, root, and rights", asked)
	}
	opened := w.open(threadID, "read")
	err = w.call("dalgona/workspace/mkdir", map[string]any{"workspaceId": opened.WorkspaceID, "path": "d", "recursive": false, "operationId": rpc.NewOperationID()}, nil)
	refusedWith(t, err, rpc.ErrorPermissionDenied)
	if _, statErr := os.Stat(filepath.Join(r.cwd, "d")); !errors.Is(statErr, os.ErrNotExist) {
		t.Fatalf("a read-only handle created a directory: %v", statErr)
	}
}

type gatedRunner struct {
	*runner.Local
	holdList  chan struct{}
	holdStart chan struct{}
	entered   chan string
}

func (g *gatedRunner) Workspace(ctx context.Context, req runner.WorkspaceRequest) (runner.WorkspaceResult, error) {
	if req.Op == runner.WorkspaceList && g.holdList != nil {
		g.entered <- "list"
		select {
		case <-g.holdList:
		case <-ctx.Done():
			return runner.WorkspaceResult{}, ctx.Err()
		}
	}
	return g.Local.Workspace(ctx, req)
}

func (g *gatedRunner) OpenWorkspace(ctx context.Context, cwd string) (runner.WorkspaceHandle, error) {
	session, err := g.Local.OpenWorkspace(ctx, cwd)
	if err != nil {
		return nil, err
	}
	return &gatedWorkspace{WorkspaceHandle: session, gate: g}, nil
}

type gatedWorkspace struct {
	runner.WorkspaceHandle
	gate *gatedRunner
}

func (h *gatedWorkspace) Workspace(ctx context.Context, req runner.WorkspaceRequest) (runner.WorkspaceResult, error) {
	if req.Op == runner.WorkspaceList && h.gate.holdList != nil {
		h.gate.entered <- "list"
		select {
		case <-h.gate.holdList:
		case <-ctx.Done():
			return runner.WorkspaceResult{}, ctx.Err()
		}
	}
	return h.WorkspaceHandle.Workspace(ctx, req)
}

func (g *gatedRunner) Start(ctx context.Context, spec runner.ProcessSpec) error {
	if g.holdStart != nil {
		g.entered <- "start"
		<-g.holdStart
	}
	return g.Local.Start(ctx, spec)
}

func gatedRig(t *testing.T) (*rig, *gatedRunner) {
	t.Helper()
	r := newRig(t, "")
	gate := &gatedRunner{entered: make(chan string, 4)}
	r.cfg.Runner = func(_ context.Context, id, _ string) (runner.Runner, error) {
		local, err := runner.NewLocal(filepath.Join(r.home, "sessions", id, "runs"))
		gate.Local = local
		return gate, err
	}
	r.openHost()
	return r, gate
}

func TestWorkspaceCloseCancelsSearchAndWaitsForCommand(t *testing.T) {
	r, gate := gatedRig(t)
	threadID := startWorkspaceThread(t, r, r.cwd)
	w := openWorkspaceWindow(t, r)
	opened := w.open(threadID, "read", "terminal")
	gate.holdList = make(chan struct{})
	searched := make(chan error, 1)
	go func() {
		searched <- w.call("dalgona/workspace/search", map[string]any{"workspaceId": opened.WorkspaceID, "query": "main"}, nil)
	}()
	<-gate.entered
	var closed workspaceClosed
	w.must("dalgona/workspace/close", map[string]any{"workspaceId": opened.WorkspaceID}, &closed)
	if closed.Status != "closed" {
		t.Fatalf("close during a search returned %q, want closed", closed.Status)
	}
	refusedWith(t, <-searched, rpc.ErrorCancelled)
	gate.holdList = nil

	commanded := w.open(threadID, "terminal")
	gate.holdStart = make(chan struct{})
	ran := make(chan error, 1)
	go func() {
		ran <- w.call("dalgona/workspace/exec", map[string]any{"workspaceId": commanded.WorkspaceID, "text": "printf done", "operationId": rpc.NewOperationID()}, nil)
	}()
	<-gate.entered
	refusedWith(t, w.call("dalgona/workspace/close", map[string]any{"workspaceId": commanded.WorkspaceID}, nil), rpc.ErrorBusy)
	close(gate.holdStart)
	if err := <-ran; err != nil {
		t.Fatalf("the command failed after a refused close: %v", err)
	}
	gate.holdStart = nil
	w.must("dalgona/workspace/close", map[string]any{"workspaceId": commanded.WorkspaceID}, &closed)
	if closed.Status != "closed" {
		t.Fatalf("close after the command returned %q, want closed", closed.Status)
	}
}

func TestWorkspaceWriteRefusesStaleRevision(t *testing.T) {
	eachWorkspaceRunner(t, func(t *testing.T, r *rig) {
		threadID := startWorkspaceThread(t, r, r.cwd)
		w := openWorkspaceWindow(t, r)
		opened := w.open(threadID, "read", "write")
		created := createWorkspaceFile(t, w, opened.WorkspaceID)
		current := divertWorkspaceFile(t, r, w, opened.WorkspaceID)
		checkStaleWriteRefused(t, w, opened.WorkspaceID, created.Revision, current.Revision)
		checkAbsentWriteRefused(t, r, w, opened.WorkspaceID)
		checkCurrentWriteAccepted(t, r, w, opened.WorkspaceID, current.Revision)
	})
}

func createWorkspaceFile(t *testing.T, w *workspaceWindow, workspaceID string) workspaceMetadata {
	t.Helper()
	created, err := w.write(workspaceID, "a.txt", runner.WorkspaceAbsent, []byte("first"))
	if err != nil {
		t.Fatalf("create: %v", err)
	}
	if got := w.stat(workspaceID, "a.txt"); got.Revision != created.Revision || got.Size != "5" {
		t.Fatalf("stat %+v does not match the committed metadata %+v", got, created)
	}
	return created
}

func divertWorkspaceFile(t *testing.T, r *rig, w *workspaceWindow, workspaceID string) workspaceMetadata {
	t.Helper()
	if writeErr := os.WriteFile(filepath.Join(r.cwd, "a.txt"), []byte("other"), 0o644); writeErr != nil {
		t.Fatal(writeErr)
	}
	return w.stat(workspaceID, "a.txt")
}

func checkStaleWriteRefused(t *testing.T, w *workspaceWindow, workspaceID, staleRevision, currentRevision string) {
	t.Helper()
	_, err := w.write(workspaceID, "a.txt", staleRevision, []byte("second"))
	if data := refusedWith(t, err, rpc.ErrorStaleRevision); data.CurrentRevision != currentRevision {
		t.Fatalf("currentRevision %q, want %q", data.CurrentRevision, currentRevision)
	}
}

func checkAbsentWriteRefused(t *testing.T, r *rig, w *workspaceWindow, workspaceID string) {
	t.Helper()
	_, err := w.write(workspaceID, "a.txt", runner.WorkspaceAbsent, []byte("third"))
	refusedWith(t, err, rpc.ErrorStaleRevision)
	if content, readErr := os.ReadFile(filepath.Join(r.cwd, "a.txt")); readErr != nil || string(content) != "other" {
		t.Fatalf("a refused write changed the file to %q (%v)", content, readErr)
	}
	leftovers, _ := filepath.Glob(filepath.Join(r.cwd, ".dalgona-upload-*"))
	if len(leftovers) != 0 {
		t.Fatalf("refused writes left staging files %v", leftovers)
	}
}

func checkCurrentWriteAccepted(t *testing.T, r *rig, w *workspaceWindow, workspaceID, currentRevision string) {
	t.Helper()
	if _, err := w.write(workspaceID, "a.txt", currentRevision, []byte("fourth")); err != nil {
		t.Fatalf("write at the current revision: %v", err)
	}
	if content, _ := os.ReadFile(filepath.Join(r.cwd, "a.txt")); string(content) != "fourth" {
		t.Fatalf("file holds %q, want fourth", content)
	}
}

func TestWorkspaceRefusesPathsOfAnotherWorkspace(t *testing.T) {
	eachWorkspaceRunner(t, func(t *testing.T, r *rig) {
		other := t.TempDir()
		if err := os.WriteFile(filepath.Join(other, "secret.txt"), []byte("secret"), 0o600); err != nil {
			t.Fatal(err)
		}
		if err := os.Symlink(other, filepath.Join(r.cwd, "escape")); err != nil {
			t.Fatal(err)
		}
		threadID := startWorkspaceThread(t, r, r.cwd)
		otherThread := startWorkspaceThread(t, r, other)
		w := openWorkspaceWindow(t, r)
		opened := w.open(threadID, "read", "write")
		read := func(rel string) error {
			return w.call("dalgona/workspace/read", map[string]any{"workspaceId": opened.WorkspaceID, "path": rel}, nil)
		}
		refusedWith(t, read("escape/secret.txt"), rpc.ErrorPermissionDenied)
		refusedWith(t, read("../"+filepath.Base(other)+"/secret.txt"), rpc.ErrorInvalidParams)
		refusedWith(t, read(filepath.Join(other, "secret.txt")), rpc.ErrorInvalidParams)
		_, err := w.write(opened.WorkspaceID, "escape/secret.txt", runner.WorkspaceAbsent, []byte("owned"))
		refusedWith(t, err, rpc.ErrorPermissionDenied)
		foreign := openWorkspaceWindow(t, r).open(otherThread, "read")
		refusedWith(t, w.call("dalgona/workspace/stat", map[string]any{"workspaceId": foreign.WorkspaceID, "path": "secret.txt"}, nil), rpc.ErrorExpired)
		var closed workspaceClosed
		w.must("dalgona/workspace/close", map[string]any{"workspaceId": foreign.WorkspaceID}, &closed)
		if closed.Status != "notFound" {
			t.Fatalf("closing a foreign handle returned %q, want notFound", closed.Status)
		}
		if content, _ := os.ReadFile(filepath.Join(other, "secret.txt")); string(content) != "secret" {
			t.Fatalf("the other workspace changed to %q", content)
		}
	})
}

func TestWorkspaceWatchReportsSameSizeEdit(t *testing.T) {
	eachWorkspaceRunner(t, func(t *testing.T, r *rig) {
		target := filepath.Join(r.cwd, "a.txt")
		if err := os.WriteFile(target, []byte("aaaa"), 0o644); err != nil {
			t.Fatal(err)
		}
		threadID := startWorkspaceThread(t, r, r.cwd)
		w := openWorkspaceWindow(t, r)
		opened := w.open(threadID, "read")
		var watching workspaceWatching
		w.must("dalgona/workspace/watch", map[string]any{"workspaceId": opened.WorkspaceID, "path": "", "recursive": false}, &watching)
		if err := os.WriteFile(target, []byte("bbbb"), 0o644); err != nil {
			t.Fatal(err)
		}
		select {
		case note := <-w.changes:
			if note.WatchID != watching.WatchID || note.RescanRequired || !slices.Equal(note.Paths, []string{"a.txt"}) || note.DirectoryRevision == watching.DirectoryRevision {
				t.Fatalf("change %+v does not report the same-size edit of a.txt", note)
			}
		case <-time.After(waitLimit):
			t.Fatal("no change notification after a same-size edit")
		}
	})
}

func execOnRemoteRunner(t *testing.T) bool {
	t.Helper()
	return strings.HasSuffix(t.Name(), "/remote")
}

func workspaceExecProbeText(t *testing.T) string {
	t.Helper()
	if runtime.GOOS == "windows" && !execOnRemoteRunner(t) {
		return "cd & exit 3"
	}
	return "pwd; exit 3"
}

func workspaceExecProbeWant(t *testing.T, r *rig) string {
	t.Helper()
	if runtime.GOOS == "windows" {
		if execOnRemoteRunner(t) {
			return msysSpelling(r.cwd) + "\n"
		}
		return r.cwd + "\r\n"
	}
	return r.cwd + "\n"
}

func workspaceExecFloodText(t *testing.T) string {
	t.Helper()
	if runtime.GOOS == "windows" && !execOnRemoteRunner(t) {
		return "for /L %i in (1,1,9000) do @echo 12345678"
	}
	return "head -c 70000 /dev/zero"
}

func msysSpelling(cwd string) string {
	if len(cwd) >= 2 && cwd[1] == ':' {
		return "/" + strings.ToLower(cwd[:1]) + strings.ReplaceAll(cwd[2:], "\\", "/")
	}
	return filepath.ToSlash(cwd)
}

func TestWorkspaceExecRunsOnTheThreadRunner(t *testing.T) {
	eachWorkspaceRunner(t, func(t *testing.T, r *rig) {
		threadID := startWorkspaceThread(t, r, r.cwd)
		w := openWorkspaceWindow(t, r)
		opened := w.open(threadID, "terminal")
		var result workspaceExecResult
		w.must("dalgona/workspace/exec", map[string]any{"workspaceId": opened.WorkspaceID, "text": workspaceExecProbeText(t), "operationId": rpc.NewOperationID()}, &result)
		want := workspaceExecProbeWant(t, r)
		if result.ExitCode == nil || *result.ExitCode != 3 || result.Termination != "exited" || string(result.OutputBase64) != want {
			t.Fatalf("exec result %+v output %q, want exit 3 in %s", result, result.OutputBase64, want)
		}
		w.must("dalgona/workspace/exec", map[string]any{"workspaceId": opened.WorkspaceID, "text": workspaceExecFloodText(t), "operationId": rpc.NewOperationID()}, &result)
		if !result.OutputTruncated || len(result.OutputBase64) != workspaceExecOutput {
			t.Fatalf("large output kept %d bytes, truncated %v; want %d and true", len(result.OutputBase64), result.OutputTruncated, workspaceExecOutput)
		}
	})
}
