//go:build gui

package manage

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"slices"
	"strconv"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
)

const chunkSize = 4

type storedFile struct {
	revision string
	data     []byte
}

type stagedUpload struct {
	path     string
	expected string
	data     []byte
}

type workspaceServer struct {
	mu        sync.Mutex
	grants    int
	files     map[string]*storedFile
	uploads   map[string]*stagedUpload
	stamp     int
	closeBusy int
	mkdirErr  error
}

func newWorkspaceServer() *workspaceServer {
	return &workspaceServer{
		files: map[string]*storedFile{
			"a.txt": {revision: "r1", data: []byte("hello world")},
			"c.txt": {revision: "rc", data: []byte("remove me")},
		},
		uploads: map[string]*stagedUpload{},
	}
}

func (w *workspaceServer) install(d *fakeDaemon) { w.installAt(d, remoteEndpoint) }

func (w *workspaceServer) installAt(d *fakeDaemon, ep client.EndpointID) {
	d.onAt(ep, methodWorkspaceOpen, w.open)
	d.onAt(ep, methodWorkspaceClose, w.close)
	d.onAt(ep, methodWorkspaceList, w.list)
	d.onAt(ep, methodWorkspaceStat, w.stat)
	d.onAt(ep, methodWorkspaceRead, w.read)
	d.onAt(ep, methodWorkspaceWrite, w.write)
	d.onAt(ep, methodWorkspaceWatch, func(call) (any, error) { return watching{WatchID: "watch-1", DirectoryRevision: "d1"}, nil })
	d.onAt(ep, methodWorkspaceUnwatch, func(call) (any, error) { return struct{}{}, nil })
	d.onAt(ep, methodWorkspaceMkdir, w.mkdir)
	d.onAt(ep, methodWorkspaceRename, func(call) (any, error) { return metadataResult{}, nil })
	d.onAt(ep, methodWorkspaceRemove, func(call) (any, error) { return struct{}{}, nil })
}

func (w *workspaceServer) open(c call) (any, error) {
	var p workspaceOpenParams
	if err := decodeParams(c, &p); err != nil {
		return nil, err
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	w.grants++
	return workspaceGrant{WorkspaceID: fmt.Sprintf("w-%d", w.grants), ThreadID: p.ThreadID, RunnerID: "local", Root: "/work/remote", PathStyle: "posix", Rights: p.Rights}, nil
}

func (w *workspaceServer) close(call) (any, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.closeBusy > 0 {
		w.closeBusy--
		return nil, rpc.NativeKind(rpc.ErrorBusy)
	}
	return workspaceClosed{Status: "closed"}, nil
}

func (w *workspaceServer) list(c call) (any, error) {
	var p listParams
	if err := decodeParams(c, &p); err != nil {
		return nil, err
	}
	if p.Path == "src" {
		return listing{DirectoryRevision: "d-src", Entries: []fileMetadata{{Name: "main.go", Kind: "file", Size: "3", Revision: "rm"}}}, nil
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	out := listing{DirectoryRevision: "d1", Entries: []fileMetadata{{Name: "src", Kind: "directory", Revision: "ds", Readable: true, Writable: true}}}
	for _, name := range []string{"a.txt", "c.txt"} {
		out.Entries = append(out.Entries, w.meta(name))
	}
	return out, nil
}

func (w *workspaceServer) meta(name string) fileMetadata {
	f := w.files[name]
	return fileMetadata{Name: name, Kind: "file", Size: strconv.Itoa(len(f.data)), Revision: f.revision, Readable: true, Writable: true}
}

func (w *workspaceServer) stat(c call) (any, error) {
	var p pathParams
	if err := decodeParams(c, &p); err != nil {
		return nil, err
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	if _, ok := w.files[p.Path]; ok {
		return w.meta(p.Path), nil
	}
	return fileMetadata{Name: p.Path, Kind: "directory", Revision: "ds", Readable: true, Writable: true}, nil
}

func (w *workspaceServer) read(c call) (any, error) {
	var p readParams
	if err := decodeParams(c, &p); err != nil {
		return nil, err
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	f, ok := w.files[p.Path]
	if !ok {
		return nil, rpc.NativeKind(rpc.ErrorNotFound)
	}
	if p.ExpectedRevision != "" && p.ExpectedRevision != f.revision {
		return nil, rpc.NativeKind(rpc.ErrorStaleRevision)
	}
	offset, err := strconv.Atoi(cmpOr(p.Offset, "0"))
	if err != nil || offset > len(f.data) {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	end := min(offset+chunkSize, len(f.data))
	return readChunk{Revision: f.revision, Total: strconv.Itoa(len(f.data)), Offset: strconv.Itoa(offset), Next: strconv.Itoa(end), DataBase64: base64.StdEncoding.EncodeToString(f.data[offset:end])}, nil
}

func (w *workspaceServer) write(c call) (any, error) {
	var stage struct {
		Stage string `json:"stage"`
	}
	if err := decodeParams(c, &stage); err != nil {
		return nil, err
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	switch stage.Stage {
	case "begin":
		var p writeBegin
		if err := decodeParams(c, &p); err != nil {
			return nil, err
		}
		id := fmt.Sprintf("u-%d", len(w.uploads)+1)
		w.uploads[id] = &stagedUpload{path: p.Path, expected: p.ExpectedRevision}
		return writeBegun{UploadID: id, Offset: "0"}, nil
	case "chunk":
		var p writeChunk
		if err := decodeParams(c, &p); err != nil {
			return nil, err
		}
		data, err := base64.StdEncoding.DecodeString(p.DataBase64)
		if err != nil {
			return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
		}
		w.uploads[p.UploadID].data = append(w.uploads[p.UploadID].data, data...)
		return struct{}{}, nil
	case "commit":
		var p writeCommit
		if err := decodeParams(c, &p); err != nil {
			return nil, err
		}
		return w.commit(w.uploads[p.UploadID])
	}
	return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
}

func (w *workspaceServer) commit(u *stagedUpload) (any, error) {
	current, exists := w.files[u.path]
	switch {
	case exists && u.expected != current.revision, !exists && u.expected != absent:
		return nil, rpc.NativeKind(rpc.ErrorStaleRevision)
	}
	w.stamp++
	w.files[u.path] = &storedFile{revision: fmt.Sprintf("r-saved-%d", w.stamp), data: u.data}
	return metadataResult{Metadata: w.meta(u.path)}, nil
}

func (w *workspaceServer) mkdir(call) (any, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.mkdirErr != nil {
		return nil, w.mkdirErr
	}
	return metadataResult{}, nil
}

func (w *workspaceServer) replace(name, revision, data string) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.files[name] = &storedFile{revision: revision, data: []byte(data)}
}

func (w *workspaceServer) content(name string) string {
	w.mu.Lock()
	defer w.mu.Unlock()
	return string(w.files[name].data)
}

func filesRig(t *testing.T) (*rig, *Files, *workspaceServer) {
	r := newRig(t)
	r.daemon.thread(remoteThread, "Remote name", "", "/work/remote")
	r.connect(localEndpoint, remoteEndpoint)
	r.open(remoteThread)
	r.thread = remoteThread
	server := newWorkspaceServer()
	server.install(r.daemon)
	p := r.panels.Files
	p.Opened(shell.Route{Panel: "files", Thread: remoteThread})
	r.frame(p)
	r.openWorkspace(p, &p.work, "Open workspace", func() bool { return p.phase == grantOpen })
	return r, p, server
}

func (r *rig) openWorkspace(p shell.Panel, w *tasks, label string, opened func() bool) {
	r.t.Helper()
	r.click(p, label)
	eventually(r.t, "the workspace opens", func() bool {
		r.frame(p)
		return opened()
	})
	r.settle(p, w)
}

func writeStages(t *testing.T, d *fakeDaemon, stage string) []call {
	t.Helper()
	var out []call
	for _, c := range d.sent(methodWorkspaceWrite) {
		var s struct {
			Stage string `json:"stage"`
		}
		if decodeParams(c, &s) == nil && s.Stage == stage {
			out = append(out, c)
		}
	}
	return out
}

func checkWorkspaceGrant(t *testing.T, r *rig) {
	t.Helper()
	checkWorkspaceOpen(t, r)
	checkWorkspaceList(t, r)
}

func checkWorkspaceOpen(t *testing.T, r *rig) {
	t.Helper()
	opens := r.daemon.sent(methodWorkspaceOpen)
	if len(opens) != 1 || opens[0].link.Endpoint != remoteEndpoint {
		t.Fatalf("workspace/open calls %+v; want one on builder", opens)
	}
	if got := params[workspaceOpenParams](t, opens[0]); got.ThreadID != "t1" || got.Root != "cwd" || !slices.Equal(got.Rights, []string{"read", "write"}) || got.OperationID == "" {
		t.Fatalf("workspace/open params %+v; want thread t1, root cwd, read and write rights, and an operation id", got)
	}
}

func checkWorkspaceList(t *testing.T, r *rig) {
	t.Helper()
	lists := r.daemon.sent(methodWorkspaceList)
	if got := params[listParams](t, lists[0]); got.WorkspaceID != "w-1" || got.Path != "" || got.Limit != listLimit {
		t.Fatalf("first listing %+v; want the root of w-1", got)
	}
	watches := r.daemon.sent(methodWorkspaceWatch)
	if len(watches) != 1 || params[watchParams](t, watches[0]) != (watchParams{WorkspaceID: "w-1"}) {
		t.Fatalf("watch calls %+v; want one non-recursive watch of the root of w-1", watches)
	}
}

func wantReads(t *testing.T, reads []call, path string) {
	t.Helper()
	if len(reads) != 3 {
		t.Fatalf("an 11-byte file in 4-byte chunks took %d reads, want 3", len(reads))
	}
	for i, c := range reads {
		checkRead(t, i, c, path)
	}
}

func checkRead(t *testing.T, i int, c call, path string) {
	t.Helper()
	got := params[readParams](t, c)
	wantRevision := "r1"
	if i == 0 {
		wantRevision = ""
	}
	if got.Path != path || got.Offset != strconv.Itoa(i*chunkSize) || got.ExpectedRevision != wantRevision {
		t.Fatalf("read %d sent %+v; want offset %d bound to revision %q", i+1, got, i*chunkSize, wantRevision)
	}
}

func checkBegins(t *testing.T, begins []call, data string) {
	t.Helper()
	if len(begins) != 2 {
		t.Fatalf("write began %d times, want the save and the explicit resolution", len(begins))
	}
	sum := sha256.Sum256([]byte(data))
	for i, want := range []string{"r1", "r2"} {
		checkWrite(t, i, begins[i], want, sum)
	}
}

func checkWrite(t *testing.T, i int, c call, wantVersion string, sum [32]byte) {
	t.Helper()
	got := params[writeBegin](t, c)
	if got.ExpectedRevision != wantVersion || got.Path != "a.txt" || got.Total != "5" || got.Digest != hex.EncodeToString(sum[:]) || got.WorkspaceID != "w-1" {
		t.Fatalf("begin %d sent %+v; want revision %s with the size and SHA-256 of the buffer", i+1, got, wantVersion)
	}
}

func checkCommits(t *testing.T, commits []call) {
	t.Helper()
	if len(commits) != 2 || params[writeCommit](t, commits[1]).OperationID == "" {
		t.Fatalf("commits %+v; each save must commit with an operation id", commits)
	}
}

func checkMkdir(t *testing.T, mkdirs []call) {
	t.Helper()
	if got := params[mkdirParams](t, mkdirs[0]); got.WorkspaceID != "w-1" || got.Path != "docs" || got.Recursive || got.OperationID == "" {
		t.Fatalf("mkdir %+v", got)
	}
}

func checkRename(t *testing.T, renames []call) {
	t.Helper()
	if got := params[renameParams](t, renames[0]); got.From != "a.txt" || got.To != "b.txt" || got.ExpectedRevision != "r1" || got.Replace || got.OperationID == "" {
		t.Fatalf("rename %+v; want a.txt to b.txt at r1 without replace", got)
	}
}

func wantRemoves(t *testing.T, removes []call) {
	t.Helper()
	want := []removeParams{{WorkspaceID: "w-1", Path: "c.txt", ExpectedRevision: "rc"}, {WorkspaceID: "w-1", Path: "src", ExpectedRevision: "ds", Recursive: true}}
	for i, want := range want {
		got := params[removeParams](t, removes[i])
		if got.OperationID == "" {
			t.Fatalf("remove %d carries no operation id", i+1)
		}
		got.OperationID = ""
		if got != want {
			t.Fatalf("remove %d sent %+v, want %+v", i+1, got, want)
		}
	}
}

func checkMutationEndpoints(t *testing.T, calls []call) {
	t.Helper()
	for _, c := range calls {
		if c.link.Endpoint != remoteEndpoint {
			t.Fatalf("%s went to %s, want builder", c.method, c.link.Endpoint)
		}
	}
}

func TestFilesOpenTheThreadWorkspaceAndReadEveryChunkAtTheFirstRevision(t *testing.T) {
	r, p, _ := filesRig(t)
	checkWorkspaceGrant(t, r)
	r.mustShow("Folder src")
	r.click(p, "File a.txt")
	r.settle(p, &p.work)
	wantReads(t, r.daemon.sent(methodWorkspaceRead), "a.txt")
	if got := p.doc.text.text(); got != "hello world" {
		t.Fatalf("loaded text %q, want the exact file bytes", got)
	}
	r.mustShow("builder: /work/remote/a.txt")
}

func TestFilesSaveDetectsAConcurrentChangeAndKeepsTheBufferUntilTheUserChooses(t *testing.T) {
	r, p, server := filesRig(t)
	r.click(p, "File a.txt")
	r.settle(p, &p.work)
	server.replace("a.txt", "r2", "theirs")
	r.typeInto(p, "File text", "mine\n")
	r.click(p, "Save")
	r.settle(p, &p.work)
	r.mustShow("The file changed since you loaded it. Choose which version to keep")
	r.mustShow("Current version on the endpoint:\ntheirs")
	if got := p.doc.text.text(); got != "mine\n" || server.content("a.txt") != "theirs" {
		t.Fatalf("after the conflict the buffer is %q and the file %q; both must stay unchanged", got, server.content("a.txt"))
	}
	r.click(p, "Keep my version")
	r.settle(p, &p.work)
	checkBegins(t, writeStages(t, r.daemon, "begin"), "mine\n")
	checkCommits(t, writeStages(t, r.daemon, "commit"))
	if server.content("a.txt") != "mine\n" {
		t.Fatalf("file holds %q after Keep my version", server.content("a.txt"))
	}
	r.mustHide("The file changed since you loaded it")
	if !slices.Contains(r.announced, platform.Announcement{Kind: platform.AnnounceCompletion, Text: "Saved a.txt on builder"}) {
		t.Fatalf("announcements %v lack the save", r.announced)
	}
}

func TestFilesTheirVersionReplacesTheBufferWithoutWriting(t *testing.T) {
	r, p, server := filesRig(t)
	r.click(p, "File a.txt")
	r.settle(p, &p.work)
	server.replace("a.txt", "r2", "theirs")
	r.typeInto(p, "File text", "mine")
	r.click(p, "Save")
	r.settle(p, &p.work)
	r.click(p, "Use their version")
	r.settle(p, &p.work)
	if got := p.doc.text.text(); got != "theirs" || p.doc.dirty() || p.doc.loaded.Revision != "r2" {
		t.Fatalf("buffer %q dirty=%v revision=%q; want their clean text at r2", got, p.doc.dirty(), p.doc.loaded.Revision)
	}
	if n := len(writeStages(t, r.daemon, "begin")); n != 1 {
		t.Fatalf("choosing their version wrote %d times; only the refused save may write", n)
	}
}

func TestFilesMutationsUseTheirNamedMethodsWithExplicitScope(t *testing.T) {
	r, p, _ := filesRig(t)
	r.typeInto(p, "New name", "docs")
	r.click(p, "Create folder")
	r.settle(p, &p.work)
	r.click(p, "File a.txt")
	r.settle(p, &p.work)
	r.click(p, "Rename this file")
	r.typeInto(p, "Rename a.txt to", "b.txt")
	r.click(p, "Rename a.txt")
	r.settle(p, &p.work)
	r.click(p, "File c.txt")
	r.settle(p, &p.work)
	r.click(p, "Remove this file")
	r.mustShow("Remove this file on builder: /work/remote/c.txt?")
	r.click(p, "Remove c.txt")
	r.settle(p, &p.work)
	r.click(p, "Folder src")
	r.settle(p, &p.work)
	r.click(p, "Remove this folder")
	r.mustShow("Remove this folder and everything inside it on builder: /work/remote/src?")
	r.click(p, "Remove src")
	r.settle(p, &p.work)
	mkdirs, renames, removes := r.daemon.sent(methodWorkspaceMkdir), r.daemon.sent(methodWorkspaceRename), r.daemon.sent(methodWorkspaceRemove)
	if len(mkdirs) != 1 || len(renames) != 1 || len(removes) != 2 {
		t.Fatalf("sent %d mkdir, %d rename, %d remove; want 1, 1, 2", len(mkdirs), len(renames), len(removes))
	}
	checkMkdir(t, mkdirs)
	checkRename(t, renames)
	wantRemoves(t, removes)
	checkMutationEndpoints(t, slices.Concat(mkdirs, renames, removes))
}

func TestFilesWatchRefreshesOnlyForItsEndpointAndKeepsAnUnsavedBuffer(t *testing.T) {
	r, p, server := filesRig(t)
	watchLink := r.daemon.sent(methodWorkspaceWatch)[0].link
	r.hub.Notified(client.Link{Endpoint: localEndpoint, Generation: 1}, methodWorkspaceChanged, []byte(`{"watchId":"watch-1","directoryRevision":"d2","paths":["new.txt"],"rescanRequired":false}`))
	r.settle(p, &p.work)
	if n := len(r.daemon.sent(methodWorkspaceList)); n != 1 {
		t.Fatalf("a change on another endpoint relisted the folder (%d listings)", n)
	}
	r.hub.Notified(watchLink, methodWorkspaceChanged, []byte(`{"watchId":"watch-1","directoryRevision":"d2","paths":["new.txt"],"rescanRequired":false}`))
	r.settle(p, &p.work)
	if n := len(r.daemon.sent(methodWorkspaceList)); n != 2 {
		t.Fatalf("a change on the watched endpoint made %d listings, want 2", n)
	}
	r.click(p, "File a.txt")
	r.settle(p, &p.work)
	reads := len(r.daemon.sent(methodWorkspaceRead))
	r.typeInto(p, "File text", "draft")
	server.replace("a.txt", "r9", "changed elsewhere")
	r.hub.Notified(watchLink, methodWorkspaceChanged, []byte(`{"watchId":"watch-1","directoryRevision":"d3","paths":["a.txt"],"rescanRequired":false}`))
	r.settle(p, &p.work)
	r.settle(p, &p.work)
	r.mustShow("This file changed on its endpoint since you loaded it")
	if got := p.doc.text.text(); got != "draft" {
		t.Fatalf("a watch update replaced the unsaved buffer with %q", got)
	}
	if n := len(r.daemon.sent(methodWorkspaceRead)); n != reads {
		t.Fatalf("a watch update reloaded the dirty file (%d reads after %d)", n, reads)
	}
}

func (w *workspaces) record(endpoint client.EndpointID) (workspaceRecord, bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	for key, rec := range w.records {
		if key.endpoint == endpoint {
			return *rec, true
		}
	}
	return workspaceRecord{}, false
}

func TestFilesKeepABusyWorkspaceHandleAndReuseItUntilCloseSucceeds(t *testing.T) {
	r, p, server := filesRig(t)
	server.closeBusy = 1
	r.click(p, "Close")
	if r.closed != 1 {
		t.Fatal("Close did not close the panel")
	}
	waitSent(t, r.daemon, methodWorkspaceClose, 1)
	eventually(t, "the busy close settles", func() bool {
		rec, ok := r.hub.workspaces.record(remoteEndpoint)
		return ok && !rec.closing && rec.grant.WorkspaceID == "w-1"
	})
	p.Opened(shell.Route{Panel: "files", Thread: remoteThread})
	r.frame(p)
	r.openWorkspace(p, &p.work, "Open workspace", func() bool { return p.phase == grantOpen })
	lists := r.daemon.sent(methodWorkspaceList)
	if n := len(r.daemon.sent(methodWorkspaceOpen)); n != 1 || params[listParams](t, lists[len(lists)-1]).WorkspaceID != "w-1" {
		t.Fatalf("after a busy close the panel opened %d handles and listed %+v; it must reuse w-1", n, lists[len(lists)-1])
	}
	r.click(p, "Close")
	waitSent(t, r.daemon, methodWorkspaceClose, 2)
	eventually(t, "the second close releases the handle", func() bool {
		_, ok := r.hub.workspaces.record(remoteEndpoint)
		return !ok
	})
	p.Opened(shell.Route{Panel: "files", Thread: remoteThread})
	r.frame(p)
	r.openWorkspace(p, &p.work, "Open workspace", func() bool { return p.phase == grantOpen })
	if n := len(r.daemon.sent(methodWorkspaceOpen)); n != 2 {
		t.Fatalf("after a successful close the next open sent %d workspace/open calls in total, want 2", n)
	}
}

func TestFilesHoldTheWorkspaceWhileAMutationOutcomeIsUnknown(t *testing.T) {
	r, p, server := filesRig(t)
	server.mkdirErr = errTransportLost
	r.typeInto(p, "New name", "docs")
	r.click(p, "Create folder")
	r.settle(p, &p.work)
	r.mustShow("Outcome unknown on builder")
	r.click(p, "Close")
	rec, ok := r.hub.workspaces.record(remoteEndpoint)
	if !ok || rec.closing || rec.owned != 1 || rec.interests != 0 {
		t.Fatalf("record %+v present=%v; the unknown mkdir must keep owning the handle", rec, ok)
	}
	if n := len(r.daemon.sent(methodWorkspaceClose)); n != 0 {
		t.Fatalf("workspace/close sent %d times before the mutation was reconciled", n)
	}
}
