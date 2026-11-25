//go:build gui

package manage

import (
	"fmt"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

var (
	localThread  = state.ThreadKey{Endpoint: localEndpoint, Thread: "t1"}
	remoteThread = state.ThreadKey{Endpoint: remoteEndpoint, Thread: "t1"}
)

func renameRig(t *testing.T) *rig {
	r := newRig(t)
	r.daemon.thread(localThread, "Local name", "", "/work/local")
	r.daemon.thread(remoteThread, "Remote name", "", "/work/remote")
	r.connect(localEndpoint, remoteEndpoint)
	r.open(localThread)
	r.open(remoteThread)
	r.thread = remoteThread
	r.panels.Rename.Opened(shell.Route{Panel: "rename", Thread: remoteThread})
	r.frame(r.panels.Rename)
	return r
}

func TestRenameSetsTheExactTitleOnlyThroughTheThreadsEndpoint(t *testing.T) {
	r := renameRig(t)
	p := r.panels.Rename
	r.mustShow("Current title: Remote name")
	r.daemon.on(methodThreadName, func(call) (any, error) { return struct{}{}, nil })
	r.typeInto(p, "New title", "Fresh  title ✓")
	r.click(p, "Rename")
	r.settle(p, &p.work)
	sent := r.daemon.sent(methodThreadName)
	if len(sent) != 1 {
		t.Fatalf("thread/name/set sent %d times, want 1", len(sent))
	}
	got := params[rpc.ThreadNameSetParams](t, sent[0])
	if sent[0].link.Endpoint != remoteEndpoint || got.ThreadID != "t1" || got.Name != "Fresh  title ✓" {
		t.Fatalf("rename went to %s with %+v; want builder, t1, exact title", sent[0].link.Endpoint, got)
	}
	if sent[0].operation == "" {
		t.Fatal("a receipt-covered rename must carry an operation identifier")
	}
	if r.closed != 1 || !slices.Contains(r.announced, platform.Announcement{Kind: platform.AnnounceCompletion, Text: "Thread renamed to Fresh  title ✓"}) {
		t.Fatalf("success must close the panel and announce; closed=%d announced=%v", r.closed, r.announced)
	}
}

func TestRenameFailureKeepsTheProposedTitle(t *testing.T) {
	r := renameRig(t)
	p := r.panels.Rename
	r.daemon.on(methodThreadName, func(call) (any, error) { return nil, rpc.NativeKind(rpc.ErrorInvalidParams) })
	r.typeInto(p, "New title", "Proposed")
	r.click(p, "Rename")
	r.settle(p, &p.work)
	if r.closed != 0 || p.name.text() != "Proposed" {
		t.Fatalf("failure closed=%d title=%q; the panel must stay open with the proposed title", r.closed, p.name.text())
	}
	r.mustShow("The daemon rejected the input as invalid.")
}

func TestRenameRefusesBlankAndUnchangedTitles(t *testing.T) {
	r := renameRig(t)
	p := r.panels.Rename
	if !r.button("Rename").Desc.Disabled {
		t.Fatal("Rename must be disabled while the title is unchanged")
	}
	r.typeInto(p, "New title", "   ")
	if !r.button("Rename").Desc.Disabled {
		t.Fatal("Rename must be disabled for a blank title")
	}
	r.click(p, "Rename")
	r.settle(p, &p.work)
	if n := len(r.daemon.sent(methodThreadName)); n != 0 {
		t.Fatalf("a blank title sent %d renames", n)
	}
	r.mustShow("Enter a title.")
}

func TestRenameUnknownOutcomeIsCheckedOnTheSameEndpoint(t *testing.T) {
	r := renameRig(t)
	p := r.panels.Rename
	r.daemon.on(methodThreadName, func(call) (any, error) { return nil, errTransportLost })
	r.daemon.on(methodOperationRead, func(call) (any, error) { return receipt{State: "succeeded", Method: methodThreadName}, nil })
	r.typeInto(p, "New title", "Maybe")
	r.click(p, "Rename")
	r.settle(p, &p.work)
	r.mustShow("Outcome unknown on builder")
	if r.closed != 0 {
		t.Fatal("an unknown outcome must not close the panel as a success")
	}
	r.click(p, "Check outcome")
	r.settle(p, &p.work)
	reads := r.daemon.sent(methodOperationRead)
	renames := r.daemon.sent(methodThreadName)
	if len(reads) != 1 || reads[0].link.Endpoint != remoteEndpoint || params[operationRead](t, reads[0]).OperationID != renames[0].operation {
		t.Fatalf("the receipt read must use the rename's operation id on builder; got %+v", reads)
	}
	if r.closed != 1 {
		t.Fatal("a succeeded receipt must finish the rename")
	}
}

func archiveRig(t *testing.T, active string) *rig {
	r := newRig(t)
	r.daemon.thread(remoteThread, "Remote name", active, "/work/remote")
	r.connect(localEndpoint, remoteEndpoint)
	r.open(remoteThread)
	r.thread = remoteThread
	return r
}

func TestArchiveWaitsForTheRunningTurnAndOffersStop(t *testing.T) {
	r := archiveRig(t, "turn-1")
	p := r.panels.Archive
	r.daemon.on(methodRemoteStatus, func(call) (any, error) { return remoteStatus{}, nil })
	p.Opened(shell.Route{Panel: "archive", Thread: remoteThread})
	r.settle(p, &p.work)
	if !r.button("Archive").Desc.Disabled {
		t.Fatal("Archive must be disabled while a turn runs")
	}
	r.click(p, "Stop turn")
	want := state.Interrupt{Window: "w", Thread: remoteThread}
	if !slices.Contains(r.submitted, state.Intent(want)) {
		t.Fatalf("Stop turn submitted %v, want %+v", r.submitted, want)
	}
	r.click(p, "Archive")
	r.settle(p, &p.work)
	if n := len(r.daemon.sent(methodThreadArchive)); n != 0 {
		t.Fatalf("a running thread was archived %d times", n)
	}
}

func TestArchiveDisclosesTheLastBoxAndPullsBeforeArchiving(t *testing.T) {
	r := archiveRig(t, "")
	p := r.panels.Archive
	r.daemon.on(methodRemoteStatus, func(c call) (any, error) {
		if params[remoteStatusParams](t, c).ThreadID != "t1" {
			t.Errorf("remote/status must name the thread")
		}
		return remoteStatus{Bindings: []remoteBinding{{ThreadID: "t1", RunnerID: "e2b", BoxID: "box-1", State: "bound", SharedThreadIDs: []string{}}}}, nil
	})
	pullGate := r.daemon.gate(methodRemotePull)
	r.daemon.on(methodRemotePull, func(call) (any, error) {
		return remotePull{ChangedPaths: []string{"main.go"}, ConflictPaths: []string{"go.mod"}}, nil
	})
	r.daemon.on(methodThreadArchive, func(call) (any, error) { return struct{}{}, nil })
	p.Opened(shell.Route{Panel: "archive", Thread: remoteThread})
	r.settle(p, &p.work)
	r.mustShow("last thread bound to box box-1")
	r.mustShow("Applying that diff changes the destination workspace")
	r.click(p, "Pull files")
	pulled := waitSent(t, r.daemon, methodRemotePull, 1)
	op := pulled[0].operation
	local := client.Link{Endpoint: localEndpoint, Generation: 1}
	r.hub.Notified(local, methodRemoteProgress, []byte(`{"operationId":"`+op+`","phase":"reading","completed":"9","total":"9"}`))
	r.hub.Notified(pulled[0].link, methodRemoteProgress, []byte(`{"operationId":"`+op+`","phase":"merging","completed":"1","total":"3"}`))
	r.frame(p)
	r.frame(p)
	r.mustShow("Pull: merging, 1 of 3")
	r.mustHide("Pull: reading")
	close(pullGate)
	r.settle(p, &p.work)
	r.mustShow("go.mod")
	r.mustShow("Conflicts left in the workspace")
	r.click(p, "Archive")
	r.settle(p, &p.work)
	archived := r.daemon.sent(methodThreadArchive)
	if len(archived) != 1 || archived[0].link.Endpoint != remoteEndpoint || params[rpc.ThreadArchiveParams](t, archived[0]).ThreadID != "t1" {
		t.Fatalf("archive calls %+v; want one on builder for t1", archived)
	}
	if r.closed != 1 {
		t.Fatal("a successful archive must close the panel")
	}
}

func checkArchivedPages(t *testing.T, r *rig, p *Archived) {
	t.Helper()
	if got := len(p.rows); got != 3 {
		t.Fatalf("rows after two pages = %d, want 3 unique", got)
	}
	lists := r.daemon.sent(methodThreadList)
	if params[rpc.ThreadListParams](t, lists[len(lists)-1]).Cursor != "c2" {
		t.Fatal("Load more must send the page cursor")
	}
}

func checkUnarchiveSent(t *testing.T, r *rig, p *Archived) {
	t.Helper()
	un := r.daemon.sent(methodThreadUnarchive)
	if len(un) != 1 || un[0].link.Endpoint != remoteEndpoint || params[threadOnly](t, un[0]).ThreadID != "c" {
		t.Fatalf("unarchive calls %+v; want c on builder", un)
	}
	if slices.ContainsFunc(p.rows, func(t rpc.Thread) bool { return t.ID == "c" }) {
		t.Fatal("an unarchived thread must leave the archived view")
	}
}

func TestArchivedViewPagesFiltersAndUnarchivesWithoutDuplicates(t *testing.T) {
	r := newRig(t)
	r.connect(localEndpoint, remoteEndpoint)
	p := r.panels.Archived
	r.daemon.onAt(remoteEndpoint, methodThreadList, func(c call) (any, error) {
		got := params[rpc.ThreadListParams](t, c)
		if !got.Archived || got.Limit != archivedPage {
			t.Errorf("archived view sent %+v; want archived:true limit:%d", got, archivedPage)
		}
		if got.Cursor == "" {
			return rpc.ThreadListResponse{Data: []rpc.Thread{{ID: "a", Name: "Alpha"}, {ID: "b", Name: "Beta"}}, NextCursor: "c2"}, nil
		}
		return rpc.ThreadListResponse{Data: []rpc.Thread{{ID: "b", Name: "Beta"}, {ID: "c", Name: "Gamma"}}}, nil
	})
	r.daemon.on(methodThreadUnarchive, func(call) (any, error) { return struct{}{}, nil })
	p.Opened(shell.Route{Panel: "archived", Thread: remoteThread})
	r.settle(p, &p.work)
	r.click(p, "Load more")
	r.settle(p, &p.work)
	checkArchivedPages(t, r, p)
	r.typeInto(p, "Filter archived threads", "gam")
	r.mustShow("Unarchive Gamma")
	r.mustHide("Unarchive Alpha")
	r.click(p, "Unarchive Gamma")
	r.settle(p, &p.work)
	checkUnarchiveSent(t, r, p)
	r.mustShow("are not restored")
}

func TestArchivedViewSwitchesEndpointAndKeepsListsApart(t *testing.T) {
	r := newRig(t)
	r.connect(localEndpoint, remoteEndpoint)
	p := r.panels.Archived
	r.daemon.onAt(localEndpoint, methodThreadList, func(call) (any, error) {
		return rpc.ThreadListResponse{Data: []rpc.Thread{{ID: "t1", Name: "Local archived"}}}, nil
	})
	r.daemon.onAt(remoteEndpoint, methodThreadList, func(call) (any, error) {
		return rpc.ThreadListResponse{Data: []rpc.Thread{{ID: "t1", Name: "Remote archived"}}}, nil
	})
	p.Opened(shell.Route{Panel: "archived", Thread: localThread})
	r.settle(p, &p.work)
	r.mustShow("Unarchive Local archived")
	r.click(p, string(remoteEndpoint)+" · Connected")
	r.settle(p, &p.work)
	r.mustShow("Unarchive Remote archived")
	r.mustHide("Unarchive Local archived")
}

func waitSent(t *testing.T, d *fakeDaemon, method string, n int) []call {
	t.Helper()
	for range 5000 {
		if got := d.sent(method); len(got) >= n {
			return got
		}
		sleepBriefly()
	}
	t.Fatalf("%s was not sent %d times", method, n)
	return nil
}

func TestRenameRefusedBeforeDispatchIsAFailureNotAnUnknownOutcome(t *testing.T) {
	r := renameRig(t)
	p := r.panels.Rename
	r.daemon.on(methodThreadName, func(call) (any, error) {
		return nil, fmt.Errorf("endpoint builder is offline: %w", client.ErrStaleLink)
	})
	r.typeInto(p, "New title", "Later")
	r.click(p, "Rename")
	r.settle(p, &p.work)
	r.mustHide("Outcome unknown")
	r.mustShow(client.ErrStaleLink.Error())
	if p.op.state != outcomeFailed || p.name.text() != "Later" {
		t.Fatalf("a refusal before dispatch left state %d and title %q", p.op.state, p.name.text())
	}
}
