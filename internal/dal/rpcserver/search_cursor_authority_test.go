package rpcserver

import (
	"context"
	json "encoding/json/v2"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

func TestTerminalAuthorityDoesNotBlockActorTerminalAccess(t *testing.T) {
	r := newRig(t, "")
	armActor := make(chan struct{})
	actorEntered := make(chan struct{})
	continueActor := make(chan struct{})
	unblock := sync.OnceFunc(func() { close(continueActor) })
	var h *leaseHarness
	clockHook := make(chan bool, 1)
	probeActor := sync.OnceFunc(func() {
		close(actorEntered)
		<-continueActor
		acquired := h.term.mu.TryLock()
		if acquired {
			h.term.mu.Unlock()
		}
		clockHook <- acquired
	})
	r.cfg.Clock = func() time.Time {
		select {
		case <-armActor:
			probeActor()
		default:
		}
		return time.Now()
	}
	r.openHost()
	threadID := startWorkspaceThread(t, r, r.cwd)
	thread, err := r.host.Resume(t.Context(), threadID)
	if err != nil {
		t.Fatal(err)
	}
	h = newLeaseHarness(t, newScriptedTerminal())
	t.Cleanup(func() { h.proc.end(0); h.term.release(); <-h.term.finished })
	t.Cleanup(unblock)
	authorityEntered := make(chan struct{})
	h.term.attachments[h.att].validate = func() error {
		close(authorityEntered)
		_, _, err := thread.BindingRevision()
		return err
	}
	close(armActor)
	appendDone := make(chan error, 1)
	go func() { appendDone <- thread.Append(t.Context(), event.ThreadNamed{Name: "probe"}) }()
	awaitAuthorityTest(t, actorEntered)
	readDone := make(chan error, 1)
	go func() { _, readErr := h.read("0", "0"); readDone <- readErr }()
	awaitAuthorityTest(t, authorityEntered)
	unblock()
	select {
	case acquired := <-clockHook:
		if !acquired {
			t.Error("actor could not inspect terminal state while terminal read waited for the binding revision")
		}
	case <-time.After(waitLimit):
		t.Fatal("actor could not complete its clock hook")
	}
	awaitAuthorityResult(t, appendDone)
	awaitAuthorityResult(t, readDone)
}

func awaitAuthorityTest(t *testing.T, reached <-chan struct{}) {
	t.Helper()
	select {
	case <-reached:
	case <-time.After(waitLimit):
		t.Fatal("actor and binding validation did not rendezvous")
	}
}

func awaitAuthorityResult(t *testing.T, result <-chan error) {
	t.Helper()
	select {
	case err := <-result:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(waitLimit):
		t.Fatal("binding validation and terminal work did not finish")
	}
}

type timedWorkspaceSearch struct {
	delayPath string
}

func (w *timedWorkspaceSearch) Workspace(ctx context.Context, req runner.WorkspaceRequest) (runner.WorkspaceResult, error) {
	if req.Op != runner.WorkspaceList {
		return runner.WorkspaceResult{}, nil
	}
	if req.Path == w.delayPath {
		<-ctx.Done()
		return runner.WorkspaceResult{}, ctx.Err()
	}
	entries := []runner.WorkspaceEntry{
		{Name: "alpha", Kind: "file"},
		{Name: "alpine", Kind: "file"},
		{Name: "alps", Kind: "file"},
		{Name: "a", Kind: "directory"},
		{Name: "b", Kind: "directory"},
	}
	if req.Path != "" {
		entries = nil
	}
	return runner.WorkspaceResult{Entries: entries, Revision: "revision:" + req.Path}, nil
}

func TestSearchCursorRechecksOnlyItsOriginalTimedCut(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		files := &timedWorkspaceSearch{delayPath: "a"}
		s := &server{}
		use := workspaceUse{ctx: t.Context(), handle: &workspaceHandle{files: files}}
		params := workspaceSearchParams{Query: "alp"}
		found, err := s.searchResults(use, params)
		if err != nil {
			t.Fatal(err)
		}
		first := issueTimedSearchPage(t, s, use.handle, found)
		if !first.Incomplete || first.NextCursor == "" {
			t.Fatalf("timed scan returned %+v, want a partial page and continuation", first)
		}
		files.delayPath = "b"
		params.Cursor = first.NextCursor
		next, err := s.searchResults(use, params)
		if err != nil {
			t.Fatalf("unchanged workspace refused its partial continuation: %v", err)
		}
		second := issueTimedSearchPage(t, s, use.handle, next)
		if !second.Incomplete || len(second.Entries) != 1 || second.Entries[0].Path == first.Entries[0].Path {
			t.Fatalf("partial continuation returned %+v after %+v", second, first)
		}
	})
}

func TestSearchCursorRemainsUsableWhenRecheckingTimesOut(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		files := &timedWorkspaceSearch{delayPath: "b"}
		s := &server{}
		use := workspaceUse{ctx: t.Context(), handle: &workspaceHandle{files: files}}
		params := workspaceSearchParams{Query: "alp"}
		found, err := s.searchResults(use, params)
		if err != nil {
			t.Fatal(err)
		}
		first := issueTimedSearchPage(t, s, use.handle, found)
		params.Cursor = first.NextCursor
		files.delayPath = "a"
		_, err = s.searchResults(use, params)
		refusedWith(t, err, rpc.ErrorUnavailable)
		files.delayPath = "b"
		next, err := s.searchResults(use, params)
		if err != nil {
			t.Fatalf("timed-out revision check consumed the cursor: %v", err)
		}
		if second := issueTimedSearchPage(t, s, use.handle, next); !second.Incomplete || len(second.Entries) != 1 {
			t.Fatalf("retried continuation returned %+v", second)
		}
	})
}

func issueTimedSearchPage(t *testing.T, s *server, h *workspaceHandle, found *workspaceCursor) workspaceSearchPage {
	t.Helper()
	raw, err := s.searchPage(h, found, 1)
	if err != nil {
		t.Fatal(err)
	}
	var page workspaceSearchPage
	if err := json.Unmarshal(raw, &page); err != nil {
		t.Fatal(err)
	}
	return page
}
