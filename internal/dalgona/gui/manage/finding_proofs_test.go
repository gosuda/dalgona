//go:build gui

package manage

import (
	"slices"
	"sync"
	"testing"

	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
)

func TestPendingTerminalLaunchStaysBoundToItsStartingThread(t *testing.T) {
	r, p, _ := terminalRig(t)
	r.daemon.thread(localThread, "Local name", "", "/work/local")
	r.open(localThread)
	gate := r.daemon.gate(methodTerminalOpen)
	r.click(p, "Start terminal")
	waitSent(t, r.daemon, methodTerminalOpen, 1)
	p.Opened(shell.Route{Panel: "terminal", Thread: localThread})
	r.frame(p)
	close(gate)
	r.settle(p, &p.work)
	if p.target != localThread {
		t.Fatalf("the panel moved to %+v, want %+v", p.target, localThread)
	}
	if p.session != nil {
		t.Fatal("a terminal started for one thread must not attach under another thread")
	}
	eventually(t, "the orphan attachment is detached", func() bool {
		return len(r.daemon.sent(methodTerminalDetach)) == 1
	})
}

func TestSaveAsConflictWritesToTheAttemptedDestination(t *testing.T) {
	r, p, server := filesRig(t)
	r.click(p, "File a.txt")
	r.settle(p, &p.work)
	r.typeInto(p, "File text", "mine\n")
	r.typeInto(p, "Save as path", "c.txt")
	r.click(p, "Save as")
	r.settle(p, &p.work)
	r.click(p, "Keep my version")
	r.settle(p, &p.work)
	if got := server.content("c.txt"); got != "mine\n" {
		t.Fatalf("after Keep my version c.txt holds %q, want the draft", got)
	}
	if p.doc.path != "c.txt" {
		t.Fatalf("the document still points at %q, want the attempted destination", p.doc.path)
	}
	if got := server.content("a.txt"); got != "hello world" {
		t.Fatalf("Keep my version overwrote the source with %q", got)
	}
}

func TestSaveAsConflictTheirVersionAdoptsTheAttemptedDestination(t *testing.T) {
	r, p, _ := filesRig(t)
	r.click(p, "File a.txt")
	r.settle(p, &p.work)
	r.typeInto(p, "File text", "mine\n")
	r.typeInto(p, "Save as path", "c.txt")
	r.click(p, "Save as")
	r.settle(p, &p.work)
	r.click(p, "Use their version")
	r.settle(p, &p.work)
	if got := p.doc.text.text(); got != "remove me" {
		t.Fatalf("after Use their version the buffer holds %q, want the destination file", got)
	}
	if p.doc.path != "c.txt" || p.doc.dirty() {
		t.Fatalf("the document points at %q dirty=%v, want a clean c.txt", p.doc.path, p.doc.dirty())
	}
	if n := len(writeStages(t, r.daemon, "begin")); n != 1 {
		t.Fatalf("choosing their version wrote %d times; only the refused save may write", n)
	}
}

func TestCheckOutcomeAfterEndpointSwitchReadsTheMutationEndpoint(t *testing.T) {
	r, p, server := filesRig(t)
	server.mkdirErr = errTransportLost
	r.daemon.on(methodOperationRead, func(call) (any, error) {
		return receipt{State: "succeeded", Method: methodWorkspaceMkdir}, nil
	})
	r.typeInto(p, "New name", "docs")
	r.click(p, "Create folder")
	r.settle(p, &p.work)
	r.mustShow("Outcome unknown on builder")
	mkdirs := r.daemon.sent(methodWorkspaceMkdir)
	if len(mkdirs) != 1 {
		t.Fatalf("mkdir sent %d times, want 1", len(mkdirs))
	}
	reads := r.daemon.sent(methodOperationRead)
	if len(reads) != 1 || reads[0].link.Endpoint != remoteEndpoint {
		t.Fatalf("receipt reads %+v; the held mkdir must reconcile on builder without a click", reads)
	}
	if params[operationRead](t, reads[0]).OperationID != p.op.id || p.op.id == "" {
		t.Fatal("the automatic receipt read must name the held mkdir operation")
	}

	r2, p2, server2 := filesRig(t)
	server2.mkdirErr = errTransportLost
	newWorkspaceServer().installAt(r2.daemon, localEndpoint)
	r2.daemon.thread(localThread, "Local name", "", "/work/local")
	r2.open(localThread)
	r2.typeInto(p2, "New name", "docs")
	r2.click(p2, "Create folder")
	r2.settle(p2, &p2.work)
	r2.mustShow("Outcome unknown on builder")
	p2.Opened(shell.Route{Panel: "files", Thread: localThread})
	r2.frame(p2)
	r2.click(p2, "Open workspace")
	eventually(t, "the local workspace opens", func() bool {
		r2.frame(p2)
		return p2.phase == grantOpen
	})
	r2.settle(p2, &p2.work)
	r2.mustHide("Check outcome")
	for _, c := range r2.daemon.sent(methodOperationRead) {
		if c.link.Endpoint != remoteEndpoint {
			t.Fatalf("a stale mutation reconciled on %s; receipts must stay on the mutation endpoint", c.link.Endpoint)
		}
	}
}

func TestCloudUpRequiresAFreshCredentialOptIn(t *testing.T) {
	r := endpointsRig(t, "e2b")
	p := r.panels.Endpoints
	r.chosen = "/work/seed"
	r.click(p, "Cloud up boxy")
	r.click(p, "Choose folder")
	r.settle(p, &p.work)
	r.click(p, "Copy this computer's provider credentials into the box")
	r.frame(p)
	r.click(p, "Start box")
	r.settle(p, &p.work)
	if len(r.life.ups) != 1 || !r.life.ups[0].CopyAuth {
		t.Fatalf("ups %+v; the opt-in must be captured for that run", r.life.ups)
	}
	r.click(p, "Cloud up boxy")
	r.frame(p)
	r.click(p, "Choose folder")
	r.settle(p, &p.work)
	r.click(p, "Start box")
	r.settle(p, &p.work)
	if len(r.life.ups) != 2 || r.life.ups[1].CopyAuth {
		t.Fatalf("ups %+v; a new confirmation must not reuse the earlier opt-in", r.life.ups)
	}
}

func TestTruncatedGarbageCollectionPreviewCannotBeConfirmed(t *testing.T) {
	r := runnerRig(t)
	p := r.panels.Runner
	r.daemon.on(methodRemoteStatus, func(call) (any, error) { return remoteStatus{}, nil })
	r.daemon.onAt(remoteEndpoint, methodRemoteGC, func(c call) (any, error) {
		return gcResult{PreviewRevision: "ledger-9", Truncated: true, Candidates: []gcCandidate{
			{BoxID: "box-a", Service: "e2b", Reason: "archived"},
			{BoxID: "box-b", Service: "e2b", Reason: "orphaned"},
		}}, nil
	})
	p.Opened(shell.Route{Panel: "runner", Thread: remoteThread})
	r.settle(p, &p.work)
	r.click(p, "Preview removable boxes")
	r.settle(p, &p.work)
	r.mustShow("did not disclose")
	r.mustHide("Remove 2 boxes on builder")
	if n := len(r.daemon.sent(methodRemoteGC)); n != 1 {
		t.Fatalf("remote/gc sent %d times; a truncated preview must never be confirmed", n)
	}
}

func TestStaleRunnerCheckNeverAppliesToANewArchiveTarget(t *testing.T) {
	r := archiveRig(t, "")
	p := r.panels.Archive
	r.daemon.thread(localThread, "Local name", "", "/work/local")
	r.open(localThread)
	var mu sync.Mutex
	calls := 0
	block := make(chan struct{})
	r.daemon.on(methodRemoteStatus, func(c call) (any, error) {
		mu.Lock()
		calls++
		first := calls == 1
		mu.Unlock()
		box := "box-local"
		if c.link.Endpoint == remoteEndpoint {
			box = "box-remote"
		}
		if first {
			<-block
		}
		var q remoteStatusParams
		if err := decodeParams(c, &q); err != nil {
			return nil, err
		}
		return remoteStatus{Bindings: []remoteBinding{{ThreadID: q.ThreadID, RunnerID: "e2b", BoxID: box, State: "bound"}}}, nil
	})
	p.Opened(shell.Route{Panel: "archive", Thread: remoteThread})
	r.frame(p)
	waitSent(t, r.daemon, methodRemoteStatus, 1)
	p.Opened(shell.Route{Panel: "archive", Thread: localThread})
	eventually(t, "the new target loads its own risk", func() bool {
		r.frame(p)
		return r.shows("box-local")
	})
	close(block)
	r.settle(p, &p.work)
	r.mustShow("box-local")
	r.mustHide("box-remote")
}

func TestFileLoadKeepsItsStartingWorkspaceAcrossPages(t *testing.T) {
	r, p, server := filesRig(t)
	var mu sync.Mutex
	var seen []string
	r.daemon.onAt(remoteEndpoint, methodWorkspaceRead, func(c call) (any, error) {
		var q readParams
		if err := decodeParams(c, &q); err != nil {
			return nil, err
		}
		mu.Lock()
		seen = append(seen, q.WorkspaceID)
		mu.Unlock()
		return server.read(c)
	})
	b := bind(r.owner.View(), remoteEndpoint)
	orig := p.lease
	for range 5 {
		mu.Lock()
		seen = nil
		mu.Unlock()
		p.doc.load(p, b, "a.txt", fileMetadata{Name: "a.txt", Revision: "r1"})
		swapped := p.lease
		swapped.grant.WorkspaceID = "w-9"
		p.lease = swapped
		r.settle(p, &p.work)
		mu.Lock()
		pages := slices.Clone(seen)
		mu.Unlock()
		if len(pages) < 2 {
			t.Fatalf("the 11-byte file needed %d page reads, want at least 2", len(pages))
		}
		for _, id := range pages {
			if id != "w-1" {
				t.Fatalf("page reads used workspace %v; every page must use the starting grant w-1", pages)
			}
		}
		p.lease = orig
	}
	if got := p.doc.text.text(); got != "hello world" {
		t.Fatalf("loaded text %q, want the exact file bytes", got)
	}
}

func TestResizeConflictRetriesWithTheLatestGeometry(t *testing.T) {
	r, p, pty := terminalRig(t)
	s := r.startTerminal(p)
	r.daemon.onAt(remoteEndpoint, methodTerminalResize, func(c call) (any, error) {
		var q terminalResizeParams
		if err := decodeParams(c, &q); err != nil {
			return nil, err
		}
		if q.ResizeSeq == "2" {
			pty.mu.Lock()
			pty.resizeSeq = 2
			pty.mu.Unlock()
			return nil, rpc.NativeKind(rpc.ErrorOperationConflict)
		}
		return pty.resize(c)
	})
	eventually(t, "the initial resize settles", func() bool {
		s.mu.Lock()
		on := s.resizeOn
		s.mu.Unlock()
		return !on
	})
	n0 := len(r.daemon.sent(methodTerminalResize))
	pty.mu.Lock()
	r0 := len(pty.resizes)
	pty.mu.Unlock()
	s.resize(100, 30)
	eventually(t, "the retry lands with a fresh sequence", func() bool {
		pty.mu.Lock()
		defer pty.mu.Unlock()
		return len(pty.resizes) == r0+1
	})
	rest := r.daemon.sent(methodTerminalResize)[n0:]
	checkResizeAttempts(t, rest)
}

func checkResizeAttempts(t *testing.T, rest []call) {
	t.Helper()
	if len(rest) < 2 {
		t.Fatalf("resize sent %d attempts after the conflict, want an attempt and a retry", len(rest))
	}
	for i := 1; i < len(rest); i++ {
		checkResizeStep(t, rest[i-1], rest[i])
	}
	for _, c := range rest {
		got := params[terminalResizeParams](t, c)
		if got.Columns != 100 || got.Rows != 30 {
			t.Fatalf("retry %+v; the coalesced attempt must carry the latest 100x30 geometry", got)
		}
	}
}

func checkResizeStep(t *testing.T, prevCall, curCall call) {
	t.Helper()
	prev, err := rpc.ParseDecimal(params[terminalResizeParams](t, prevCall).ResizeSeq)
	if err != nil {
		t.Fatal(err)
	}
	cur, err := rpc.ParseDecimal(params[terminalResizeParams](t, curCall).ResizeSeq)
	if err != nil {
		t.Fatal(err)
	}
	if cur != prev+1 {
		t.Fatalf("resize sequences must advance by exactly one after the conflict")
	}
}

func TestExpiredWorkspaceReopensWithAFreshGrant(t *testing.T) {
	r, p, server := filesRig(t)
	r.daemon.onAt(remoteEndpoint, methodWorkspaceList, func(c call) (any, error) {
		var q listParams
		if err := decodeParams(c, &q); err != nil {
			return nil, err
		}
		if q.WorkspaceID == "w-1" {
			return nil, rpc.NativeKind(rpc.ErrorExpired)
		}
		return server.list(c)
	})
	r.click(p, "Refresh")
	r.settle(p, &p.work)
	opens := len(r.daemon.sent(methodWorkspaceOpen))
	r.click(p, "Open workspace")
	eventually(t, "the expired workspace asks for a fresh grant", func() bool {
		r.frame(p)
		return len(r.daemon.sent(methodWorkspaceOpen)) == opens+1
	})
	r.settle(p, &p.work)
	r.mustShow("Folder src")
}

func TestCancelledSignInResolvesThroughAuthoritativeStatus(t *testing.T) {
	r, p := serversRig(t, signInFixture())
	r.mcpProviders(func() bool { return false })
	r.daemon.onAt(remoteEndpoint, methodAuthStart, func(call) (any, error) {
		return authStatus{TransactionID: "tx-7", Target: zetaTarget(), State: "awaitingInput", Challenge: &authChallenge{Kind: "manual"}}, nil
	})
	r.daemon.onAt(remoteEndpoint, methodAuthCancel, func(call) (any, error) {
		return struct {
			Status string `json:"status"`
		}{Status: "completed"}, nil
	})
	r.daemon.onAt(remoteEndpoint, methodAuthStatus, func(call) (any, error) {
		return authStatus{TransactionID: "tx-7", Target: zetaTarget(), State: "failed"}, nil
	})
	r.click(p, "Sign-in for zeta")
	r.settle(p, &p.work)
	r.click(p, "Sign in with Paste the redirect or code")
	r.settle(p, &p.work)
	r.typeInto(p, "Redirect URL or authorization code", "code-123")
	r.click(p, "Cancel sign-in")
	r.settle(p, &p.work)
	if got := p.auth.input.text(); got != "" {
		t.Fatalf("a cancelled sign-in kept %q; the fields must clear", got)
	}
	r.mustShow("Sign-in failed")
	if n := len(r.daemon.sent(methodAuthSubmit)); n != 0 {
		t.Fatalf("a cancelled sign-in submitted %d responses", n)
	}
}

func TestBrowserOpensOncePerAuthorizationChallenge(t *testing.T) {
	r, p := serversRig(t, signInFixture())
	browsed := 0
	r.hub.deps.Browse = func(string) error { browsed++; return nil }
	b := bind(r.owner.View(), remoteEndpoint)
	p.auth.bind(zetaTarget(), "zeta")
	p.auth.flow = flowBrowser
	st := authStatus{TransactionID: "tx-b", Target: zetaTarget(), State: "awaitingInput", Challenge: &authChallenge{Kind: "browserCallback", AuthorizationURL: "https://idp.example/device"}}
	p.auth.accept(r.hub, &p.work, b, st)
	p.auth.accept(r.hub, &p.work, b, st)
	if browsed != 1 {
		t.Fatalf("the same challenge opened the browser %d times, want exactly 1", browsed)
	}
}

func TestUncertainTerminalStopOffersACheckOutcome(t *testing.T) {
	r, p, _ := terminalRig(t)
	r.startTerminal(p)
	r.daemon.onAt(remoteEndpoint, methodTerminalClose, func(call) (any, error) {
		return nil, errTransportLost
	})
	r.daemon.on(methodOperationRead, func(call) (any, error) {
		return receipt{State: "succeeded", Method: methodTerminalClose}, nil
	})
	r.click(p, "Stop terminal")
	r.settle(p, &p.work)
	stops := r.daemon.sent(methodTerminalClose)
	if len(stops) != 1 {
		t.Fatalf("terminal/close sent %d times, want 1", len(stops))
	}
	r.mustShow("Outcome unknown")
	r.click(p, "Check outcome")
	r.settle(p, &p.work)
	reads := r.daemon.sent(methodOperationRead)
	if len(reads) != 1 || reads[0].link.Endpoint != remoteEndpoint {
		t.Fatalf("receipt reads %+v; want one on builder", reads)
	}
	if params[operationRead](t, reads[0]).OperationID == "" {
		t.Fatal("the receipt read must name the stop operation")
	}
}

func TestReattachStopsTheSupersededWriter(t *testing.T) {
	r, p, pty := terminalRig(t)
	s := r.startTerminal(p)
	r.daemon.onAt(remoteEndpoint, methodTerminalRead, func(c call) (any, error) {
		if c.link.Generation == 1 {
			return nil, errTransportLost
		}
		return pty.read(c)
	})
	if err := r.daemon.Disconnect(remoteEndpoint); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the reader reports the lost transport", func() bool { return s.snapshot().end == endLost })
	r.connect(remoteEndpoint)
	eventually(t, "the terminal reattaches", func() bool {
		r.frame(p)
		return p.session != nil && p.session != s
	})
	eventually(t, "the old writer stops", func() bool {
		select {
		case <-s.readerDone:
		default:
			return false
		}
		select {
		case <-s.writerDone:
		default:
			return false
		}
		return true
	})
	r.focus(p, &p.viewport)
	r.router.Queue(key.EditEvent{Text: "b"})
	r.frame(p)
	r.awaitWrites(pty, "b")
	pty.check(t)
}

func TestFailedReattachKeepsTheOldTerminalView(t *testing.T) {
	r, p, pty := terminalRig(t)
	s := r.startTerminal(p)
	pty.emit("motd\r\n$ ")
	r.daemon.onAt(remoteEndpoint, methodTerminalRead, func(c call) (any, error) {
		if c.link.Generation == 1 {
			return nil, errTransportLost
		}
		return pty.read(c)
	})
	r.daemon.onAt(remoteEndpoint, methodTerminalAttach, func(call) (any, error) {
		return nil, rpc.NativeKind(rpc.ErrorBusy)
	})
	if err := r.daemon.Disconnect(remoteEndpoint); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the reader reports the lost transport", func() bool { return s.snapshot().end == endLost })
	r.connect(remoteEndpoint)
	eventually(t, "the failed reattach surfaces", func() bool {
		r.frame(p)
		return p.startErr != nil
	})
	if p.session != s {
		t.Fatal("a failed reattach must keep the old session and its output")
	}
	if p.startErr == nil {
		t.Fatal("a failed reattach must surface its error")
	}
}

func TestManualCodeSubmitAllowedWhileWaiting(t *testing.T) {
	r, p := serversRig(t, signInFixture())
	done := false
	r.mcpProviders(func() bool { return done })
	r.daemon.onAt(remoteEndpoint, methodAuthStart, func(call) (any, error) {
		return authStatus{TransactionID: "tx-1", Target: zetaTarget(), State: "waiting", Challenge: &authChallenge{Kind: "manual"}}, nil
	})
	r.daemon.onAt(remoteEndpoint, methodAuthSubmit, func(call) (any, error) {
		done = true
		return authStatus{TransactionID: "tx-1", Target: zetaTarget(), State: "succeeded"}, nil
	})
	r.click(p, "Sign-in for zeta")
	r.settle(p, &p.work)
	r.click(p, "Sign in with Paste the redirect or code")
	eventually(t, "the manual challenge appears", func() bool {
		r.frame(p)
		return p.auth.status != nil
	})
	if !p.auth.submittable() {
		t.Fatal("a waiting manual challenge must allow submission")
	}
	r.typeInto(p, "Redirect URL or authorization code", "code-123")
	r.click(p, "Submit Redirect URL or authorization code")
	eventually(t, "the code is submitted", func() bool {
		r.frame(p)
		return len(r.daemon.sent(methodAuthSubmit)) == 1
	})
	checkAuthSubmit(t, r.daemon.sent(methodAuthSubmit))
}
