//go:build gui

package manage

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type diffRouteCase struct {
	thread      state.ThreadKey
	workspaceID string
	file        string
	opens       int
	stats       int
	reads       int
}

func checkDiffWorkspaceOpen(t *testing.T, r *rig, want diffRouteCase) {
	t.Helper()
	gotOpens := r.daemon.sent(methodWorkspaceOpen)
	if len(gotOpens) != want.opens+1 {
		t.Fatalf("got %d workspace opens; want %d", len(gotOpens), want.opens+1)
	}
	if c := gotOpens[want.opens]; c.link.Endpoint != want.thread.Endpoint || params[workspaceOpenParams](t, c).ThreadID != want.thread.Thread {
		t.Fatalf("workspace open on %s with %s; want %+v", c.link.Endpoint, c.params, want.thread)
	}
}

func checkDiffWorkspaceStat(t *testing.T, r *rig, want diffRouteCase) {
	t.Helper()
	for _, c := range r.daemon.sent(methodWorkspaceStat)[want.stats:] {
		if params[pathParams](t, c) == (pathParams{WorkspaceID: want.workspaceID, Path: want.file}) && c.link.Endpoint == want.thread.Endpoint {
			return
		}
	}
	t.Fatalf("no workspace stat of %q on %s using %s", want.file, want.thread.Endpoint, want.workspaceID)
}

func checkDiffWorkspaceReads(t *testing.T, r *rig, want diffRouteCase) {
	t.Helper()
	gotReads := r.daemon.sent(methodWorkspaceRead)[want.reads:]
	if len(gotReads) == 0 {
		t.Fatal("no read of the requested file")
	}
	for _, c := range gotReads {
		if p := params[readParams](t, c); p.Path != want.file || p.WorkspaceID != want.workspaceID || c.link.Endpoint != want.thread.Endpoint {
			t.Fatalf("read on endpoint %q with %+v; want %q using %q on %q", c.link.Endpoint, p, want.file, want.workspaceID, want.thread.Endpoint)
		}
	}
}

func checkDiffWorkspaceRoute(t *testing.T, r *rig, want diffRouteCase) {
	t.Helper()
	checkDiffWorkspaceOpen(t, r, want)
	checkDiffWorkspaceStat(t, r, want)
	checkDiffWorkspaceReads(t, r, want)
}

type diffRouteFixture struct {
	r          *rig
	p          *Files
	diffThread state.ThreadKey
	file       string
	route      shell.Route
	waitFile   func(*testing.T, string)
	stats      int
	reads      int
	remote     *workspaceServer
}

func loadDiffRouteWorkspace(t *testing.T, f *diffRouteFixture) {
	t.Helper()
	f.p.Opened(f.route)
	f.waitFile(t, "remote diff file")
	f.r.mustShow("builder: /work/remote/" + f.file)
	checkDiffWorkspaceRoute(t, f.r, diffRouteCase{thread: f.diffThread, workspaceID: "w-1", file: f.file})
	f.stats = len(f.r.daemon.sent(methodWorkspaceStat))
	f.reads = len(f.r.daemon.sent(methodWorkspaceRead))
}

func rejectDiffRouteOutsideGrant(t *testing.T, f *diffRouteFixture) {
	t.Helper()
	for _, refusal := range []struct{ path, reason string }{
		{"/work/remote-sibling/" + f.file, "outside the granted workspace"},
		{"/work/remote/src/../a.txt", "does not name a workspace file"},
		{"../a.txt", "does not name a workspace file"},
	} {
		bad := f.route
		bad.Context = refusal.path
		f.p.Opened(bad)
		f.r.settle(f.p, &f.p.work)
		f.r.mustShow(refusal.reason)
	}
	if len(f.r.daemon.sent(methodWorkspaceStat)) != f.stats || len(f.r.daemon.sent(methodWorkspaceRead)) != f.reads {
		t.Fatal("a wrong-root or traversal route reached workspace stat or read")
	}
}

func holdDiffRouteUnsavedDraft(t *testing.T, f *diffRouteFixture) {
	t.Helper()
	f.r.typeInto(f.p, "File text", "unsaved draft")
	f.p.Opened(f.route)
	f.r.settle(f.p, &f.p.work)
	f.r.mustHide("has unsaved changes")
	f.p.Opened(shell.Route{Panel: "files", Thread: f.diffThread, Context: "c.txt"})
	f.r.frame(f.p)
	f.r.mustShow("has unsaved changes")
	f.r.click(f.p, "Cancel")
	f.r.settle(f.p, &f.p.work)
	if got := f.p.doc.text.text(); got != "unsaved draft" {
		t.Fatalf("cancel left editor text %q; want the unchanged draft", got)
	}
	if len(f.r.daemon.sent(methodWorkspaceStat)) != f.stats || len(f.r.daemon.sent(methodWorkspaceRead)) != f.reads {
		t.Fatal("the cancelled file route reached workspace stat or read")
	}
}

func moveDiffRouteAcrossWorkspace(t *testing.T, f *diffRouteFixture) {
	t.Helper()
	f.p.Opened(shell.Route{Panel: "files", Thread: localThread, Context: `D:\sandbox\src\notes.txt`})
	f.r.frame(f.p)
	f.r.mustShow("has unsaved changes")
	if len(f.r.daemon.sent(methodWorkspaceOpen)) != 1 || len(f.r.daemon.sent(methodWorkspaceClose)) != 0 {
		t.Fatal("the workspace changed before the unsaved-file decision")
	}
	f.r.click(f.p, "Save changes")
	f.waitFile(t, "local diff file")
	f.r.mustShow(`D:\sandbox\src\notes.txt`)
	if got := f.remote.content(f.file); got != "unsaved draft" {
		t.Fatalf("remote workspace contains %q after Save changes; want the draft", got)
	}
	checkDiffWorkspaceRoute(t, f.r, diffRouteCase{thread: localThread, workspaceID: "local-grant", file: f.file, opens: 1, stats: f.stats, reads: f.reads})
}

func TestFilesOpenDiffRouteOnOwningWorkspace(t *testing.T) {
	r := newRig(t)
	diffThread := state.ThreadKey{Endpoint: remoteEndpoint, Thread: "diff-thread"}
	r.daemon.thread(diffThread, "Diff thread", "", "/work/remote")
	r.daemon.thread(localThread, "Other thread", "", `D:\sandbox`)
	r.connect(localEndpoint, remoteEndpoint)
	r.open(localThread)
	r.thread = localThread

	const file = "src/notes.txt"
	remote, local := newWorkspaceServer(), newWorkspaceServer()
	remote.replace(file, "remote-rev", "remote diff file")
	local.replace(file, "local-rev", "local diff file")
	remote.install(r.daemon)
	local.installAt(r.daemon, localEndpoint)
	r.daemon.onAt(localEndpoint, methodWorkspaceOpen, func(c call) (any, error) {
		var p workspaceOpenParams
		if err := decodeParams(c, &p); err != nil {
			return nil, err
		}
		return workspaceGrant{WorkspaceID: "local-grant", ThreadID: p.ThreadID, Root: `D:\sandbox`, PathStyle: "windows", Rights: p.Rights}, nil
	})
	p := r.panels.Files
	waitFile := func(t *testing.T, want string) {
		t.Helper()
		eventually(t, "the diff route loads "+want, func() bool {
			r.settle(p, &p.work)
			return p.doc.text.text() == want
		})
		r.settle(p, &p.work)
	}

	route := shell.Route{Panel: "files", Thread: diffThread, Context: "/work/remote/" + file}
	fixture := &diffRouteFixture{r: r, p: p, diffThread: diffThread, file: file, route: route, waitFile: waitFile, remote: remote}
	t.Run("loadsOwningWorkspace", func(t *testing.T) {
		loadDiffRouteWorkspace(t, fixture)
	})
	t.Run("rejectsOutsideGrant", func(t *testing.T) {
		rejectDiffRouteOutsideGrant(t, fixture)
	})
	t.Run("holdsUnsavedDraft", func(t *testing.T) {
		holdDiffRouteUnsavedDraft(t, fixture)
	})
	t.Run("movesAcrossWorkspace", func(t *testing.T) {
		moveDiffRouteAcrossWorkspace(t, fixture)
	})
}
