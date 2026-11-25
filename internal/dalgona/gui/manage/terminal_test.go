//go:build gui

package manage

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"slices"
	"strconv"
	"sync"
	"testing"
	"time"

	"gioui.org/io/input"
	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/vtstate"
)

const (
	ptyColumns    = 80
	ptyRows       = 24
	checkpointCut = 512
	farFuture     = "2099-01-01T00:00:00.000Z"
)

type ptyServer struct {
	mu         sync.Mutex
	done       chan struct{}
	workspaces *workspaceServer
	out        []byte
	resizes    []terminalResize
	status     terminalStatus
	retained   int
	hold       bool
	lease      string
	attachment int
	inputSeq   uint64
	resizeSeq  uint64
	writes     []string
	writeErr   error
	bodies     map[string][]byte
	listing    []terminalListing
	problems   []string
}

func newPTY() *ptyServer {
	return &ptyServer{
		done: make(chan struct{}), workspaces: newWorkspaceServer(), lease: "lease-1",
		status: terminalStatus{State: "running", InputState: "ready"}, bodies: map[string][]byte{},
	}
}

func (p *ptyServer) install(d *fakeDaemon) {
	d.onAt(remoteEndpoint, methodWorkspaceOpen, p.workspaces.open)
	d.onAt(remoteEndpoint, methodWorkspaceClose, p.workspaces.close)
	d.onAt(remoteEndpoint, methodTerminalList, func(call) (any, error) {
		p.mu.Lock()
		defer p.mu.Unlock()
		return terminalList{Terminals: p.listing}, nil
	})
	d.onAt(remoteEndpoint, methodTerminalOpen, func(call) (any, error) { return p.opened(true), nil })
	d.onAt(remoteEndpoint, methodTerminalAttach, p.attach)
	d.onAt(remoteEndpoint, methodCheckpointOpen, func(call) (any, error) {
		p.mu.Lock()
		defer p.mu.Unlock()
		return checkpointOpened{Checkpoint: p.checkpointLocked(), Status: p.status}, nil
	})
	d.onAt(remoteEndpoint, methodCheckpointRead, p.checkpointRead)
	d.onAt(remoteEndpoint, methodTerminalRead, p.read)
	d.onAt(remoteEndpoint, methodTerminalWrite, p.write)
	d.onAt(remoteEndpoint, methodTerminalResize, p.resize)
	d.onAt(remoteEndpoint, methodTerminalClose, func(call) (any, error) {
		p.mu.Lock()
		defer p.mu.Unlock()
		zero := 0
		p.status = terminalStatus{State: "exited", InputState: "closed", ExitCode: &zero}
		return terminalClosed{Status: p.status}, nil
	})
	d.onAt(remoteEndpoint, methodTerminalDetach, func(call) (any, error) { return terminalDetached{Status: "detached"}, nil })
}

func (p *ptyServer) problem(format string, args ...any) {
	p.problems = append(p.problems, fmt.Sprintf(format, args...))
}

func (p *ptyServer) identity() terminalIdentity {
	return terminalIdentity{TerminalID: "term-1", DaemonInstanceID: "daemon-1", ThreadID: "t1", RunnerID: "local", BindingRevision: "b1", Root: "/work/remote"}
}

func (p *ptyServer) opened(writer bool) terminalOpened {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.attachment++
	att := terminalAttachment{
		AttachmentID: fmt.Sprintf("att-%d", p.attachment), WorkspaceID: "w-1",
		AcceptedInputSeq: rpc.FormatDecimal(p.inputSeq), AcceptedResizeSeq: rpc.FormatDecimal(p.resizeSeq), LeaseExpiresAt: farFuture,
	}
	if writer {
		att.WriterLease = p.lease
	}
	return terminalOpened{Identity: p.identity(), Attachment: att, Checkpoint: p.checkpointLocked(), Status: p.status}
}

func (p *ptyServer) attach(c call) (any, error) {
	var req attachParams
	if err := decodeParams(c, &req); err != nil {
		return nil, err
	}
	return p.opened(req.Access == "write"), nil
}

func (p *ptyServer) checkpointLocked() terminalCheckpoint {
	term := vtstate.New(ptyColumns, ptyRows)
	at := 0
	for _, r := range p.resizes {
		offset, _ := strconv.Atoi(r.Offset)
		term.Feed(p.out[at:offset])
		at = offset
		term.Resize(r.Columns, r.Rows)
	}
	term.Feed(p.out[at:])
	body, err := term.Checkpoint()
	if err != nil {
		p.problem("checkpoint: %v", err)
	}
	id := fmt.Sprintf("cp-%d", len(p.bodies)+1)
	p.bodies[id] = body
	sum := sha256.Sum256(body)
	columns, rows := term.Size()
	return terminalCheckpoint{
		CheckpointID: id, Version: vtstate.Version, EngineVersion: vtstate.EngineVersion(),
		NextOffset: strconv.Itoa(len(p.out)), ResizeOrdinal: strconv.Itoa(len(p.resizes)), Columns: columns, Rows: rows,
		Total: strconv.Itoa(len(body)), Digest: hex.EncodeToString(sum[:]), ExpiresAt: farFuture,
	}
}

func (p *ptyServer) checkpointRead(c call) (any, error) {
	var req checkpointReadParams
	if err := decodeParams(c, &req); err != nil {
		return nil, err
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	body, ok := p.bodies[req.CheckpointID]
	offset, err := strconv.Atoi(cmpOr(req.Offset, "0"))
	if !ok || err != nil || offset > len(body) {
		return nil, rpc.NativeKind(rpc.ErrorExpired)
	}
	end := min(offset+checkpointCut, len(body))
	sum := sha256.Sum256(body)
	return checkpointChunk{Offset: strconv.Itoa(offset), Next: strconv.Itoa(end), Total: strconv.Itoa(len(body)), Digest: hex.EncodeToString(sum[:]), DataBase64: base64.StdEncoding.EncodeToString(body[offset:end])}, nil
}

func (p *ptyServer) read(c call) (any, error) {
	var req terminalReadParams
	if err := decodeParams(c, &req); err != nil {
		return nil, err
	}
	deadline := time.Now().Add(time.Second)
	for {
		page, ready, err := p.page(req)
		if err != nil || ready || time.Now().After(deadline) {
			return page, err
		}
		select {
		case <-p.done:
			return page, nil
		case <-time.After(2 * time.Millisecond):
		}
	}
}

func (p *ptyServer) page(req terminalReadParams) (terminalPage, bool, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	offset, err := strconv.Atoi(req.Offset)
	after, err2 := strconv.Atoi(req.AfterResizeOrdinal)
	if err != nil || err2 != nil || offset > len(p.out) || after > len(p.resizes) {
		p.problem("read with an impossible cursor pair %+v", req)
		return terminalPage{}, false, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	if offset < p.retained {
		return terminalPage{}, false, rpc.NativeKind(rpc.ErrorReplayGap)
	}
	page := terminalPage{Offset: req.Offset, Next: req.Offset, ResizeOrdinal: req.AfterResizeOrdinal, Status: p.status, LeaseExpiresAt: farFuture}
	if p.hold || offset == len(p.out) && after == len(p.resizes) {
		return page, false, nil
	}
	page.Next, page.ResizeOrdinal = strconv.Itoa(len(p.out)), strconv.Itoa(len(p.resizes))
	page.DataBase64, page.Resizes = base64.StdEncoding.EncodeToString(p.out[offset:]), slices.Clone(p.resizes[after:])
	return page, true, nil
}

func (p *ptyServer) write(c call) (any, error) {
	var req terminalWriteParams
	if err := decodeParams(c, &req); err != nil {
		return nil, err
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.writeErr != nil {
		return nil, p.writeErr
	}
	seq, err := rpc.ParseDecimal(req.InputSeq)
	if req.WriterLease != p.lease || err != nil || seq != p.inputSeq+1 {
		p.problem("write %+v does not follow lease %s sequence %d", req, p.lease, p.inputSeq)
		return nil, rpc.NativeKind(rpc.ErrorOperationConflict)
	}
	data, err := base64.StdEncoding.DecodeString(req.DataBase64)
	if err != nil {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	p.inputSeq = seq
	p.writes = append(p.writes, string(data))
	return terminalAccepted{AcceptedSeq: req.InputSeq}, nil
}

func (p *ptyServer) resize(c call) (any, error) {
	var req terminalResizeParams
	if err := decodeParams(c, &req); err != nil {
		return nil, err
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	seq, err := rpc.ParseDecimal(req.ResizeSeq)
	if req.WriterLease != p.lease || err != nil || seq != p.resizeSeq+1 {
		p.problem("resize %+v does not follow lease %s sequence %d", req, p.lease, p.resizeSeq)
		return nil, rpc.NativeKind(rpc.ErrorOperationConflict)
	}
	p.resizeSeq = seq
	p.resizes = append(p.resizes, terminalResize{Ordinal: strconv.Itoa(len(p.resizes) + 1), Offset: strconv.Itoa(len(p.out)), Columns: req.Columns, Rows: req.Rows})
	return terminalAccepted{AcceptedSeq: req.ResizeSeq}, nil
}

func (p *ptyServer) emit(s string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.out = append(p.out, s...)
}

func (p *ptyServer) finalize(tail string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.out = append(p.out, tail...)
	p.status.FinalOffset, p.status.FinalResizeOrdinal, p.status.OutputExpiresAt = strconv.Itoa(len(p.out)), strconv.Itoa(len(p.resizes)), farFuture
}

func (p *ptyServer) written() []string {
	p.mu.Lock()
	defer p.mu.Unlock()
	return slices.Clone(p.writes)
}

func (p *ptyServer) check(t *testing.T) {
	t.Helper()
	p.mu.Lock()
	defer p.mu.Unlock()
	if len(p.problems) > 0 {
		t.Fatalf("the terminal server saw protocol violations: %q", p.problems)
	}
}

func terminalRig(t *testing.T) (*rig, *Terminal, *ptyServer) {
	r := newRig(t)
	r.daemon.thread(remoteThread, "Remote name", "", "/work/remote")
	r.connect(localEndpoint, remoteEndpoint)
	r.open(remoteThread)
	r.thread = remoteThread
	pty := newPTY()
	pty.install(r.daemon)
	p := r.panels.Terminal
	t.Cleanup(func() {
		close(pty.done)
		if s := p.session; s != nil {
			s.stopReading()
			<-s.readerDone
		}
	})
	p.Opened(shell.Route{Panel: "terminal", Thread: remoteThread})
	r.frame(p)
	r.openWorkspace(p, &p.work, "Open workspace for a terminal", func() bool { return p.phase == grantOpen })
	return r, p, pty
}

func (r *rig) startTerminal(p *Terminal) *termSession {
	r.t.Helper()
	r.click(p, "Start terminal")
	r.settle(p, &p.work)
	if p.session == nil {
		r.t.Fatalf("the terminal did not start: %v", p.startErr)
	}
	r.frame(p)
	return p.session
}

func (r *rig) awaitWrites(pty *ptyServer, want ...string) {
	r.t.Helper()
	eventually(r.t, fmt.Sprintf("the terminal receives %q", want), func() bool { return slices.Equal(pty.written(), want) })
}

func checkTerminalGrant(t *testing.T, r *rig) {
	t.Helper()
	opens := r.daemon.sent(methodWorkspaceOpen)
	if len(opens) != 1 || opens[0].link.Endpoint != remoteEndpoint || !slices.Equal(params[workspaceOpenParams](t, opens[0]).Rights, terminalRights) {
		t.Fatalf("workspace/open calls %+v; want one terminal grant on builder", opens)
	}
}

func checkTerminalStart(t *testing.T, r *rig) {
	t.Helper()
	starts := r.daemon.sent(methodTerminalOpen)
	if len(starts) != 1 || starts[0].link.Endpoint != remoteEndpoint {
		t.Fatalf("terminal/open calls %+v; want one on builder", starts)
	}
	got := params[terminalOpenParams](t, starts[0])
	if got.WorkspaceID != "w-1" || got.OperationID == "" || got.Columns < 20 || got.Columns > 240 || got.Rows < 5 || got.Rows > 120 {
		t.Fatalf("terminal/open params %+v; want the granted workspace, an operation id, and a grid inside the protocol bounds", got)
	}
}

func wantCheckpointReads(t *testing.T, r *rig, pty *ptyServer) {
	t.Helper()
	reads := r.daemon.sent(methodCheckpointRead)
	pty.mu.Lock()
	chunks := (len(pty.bodies["cp-1"]) + checkpointCut - 1) / checkpointCut
	pty.mu.Unlock()
	if len(reads) != chunks || chunks < 2 {
		t.Fatalf("checkpoint read %d times for %d chunks; the body must be downloaded whole before import", len(reads), chunks)
	}
	for i, c := range reads {
		checkCheckpointRead(t, i, c)
	}
}

func checkCheckpointRead(t *testing.T, i int, c call) {
	t.Helper()
	req := params[checkpointReadParams](t, c)
	if req.CheckpointID != "cp-1" || req.AttachmentID != "att-1" || req.Offset != strconv.Itoa(i*checkpointCut) {
		t.Fatalf("checkpoint read %d sent %+v", i+1, req)
	}
}

func checkReplicaCursor(t *testing.T, s *termSession, want uint64) {
	t.Helper()
	s.mu.Lock()
	next, _ := s.replica.Cursors()
	s.mu.Unlock()
	if next != want {
		t.Fatalf("the replica starts at offset %d, want the checkpoint cursor", next)
	}
}

func checkReattach(t *testing.T, r *rig) {
	t.Helper()
	attaches := r.daemon.sent(methodTerminalAttach)
	if len(attaches) != 1 || attaches[0].link.Generation != 2 {
		t.Fatalf("attach calls %+v; want one on the new connection", attaches)
	}
	got := params[attachParams](t, attaches[0])
	if got.TerminalID != "term-1" || got.Access != "write" || got.ResumeLease != "lease-1" || got.WorkspaceID != "w-2" {
		t.Fatalf("attach %+v; want term-1 with resume lease lease-1 through a fresh workspace handle", got)
	}
}

func checkResequenced(t *testing.T, r *rig) {
	t.Helper()
	writes := r.daemon.sent(methodTerminalWrite)
	if last := params[terminalWriteParams](t, writes[len(writes)-1]); last.InputSeq != "2" {
		t.Fatalf("the first write after reattachment used sequence %s, want 2", last.InputSeq)
	}
}

func TestTerminalOpensOnTheThreadsEndpointAndRestoresTheChunkedCheckpoint(t *testing.T) {
	r, p, pty := terminalRig(t)
	pty.emit("motd\r\n$ ")
	r.mustShow("Shell: " + terminalTarget)
	r.mustShow("Endpoint: builder")
	checkTerminalGrant(t, r)
	s := r.startTerminal(p)
	checkTerminalStart(t, r)
	wantCheckpointReads(t, r, pty)
	checkReplicaCursor(t, s, uint64(len("motd\r\n$ ")))
	r.mustShow("Running · keys go to the terminal while it has focus")
	pty.check(t)
}

func TestTerminalWritesFocusedKeysInSequenceWithItsLeaseAndKeepsTab(t *testing.T) {
	r, p, pty := terminalRig(t)
	r.startTerminal(p)
	r.focus(p, &p.viewport)
	r.router.Queue(key.EditEvent{Text: "ls"})
	r.frame(p)
	r.key(p, key.NameReturn, 0)
	r.router.Queue(input.SystemEvent{Event: key.Event{Name: key.NameTab, State: key.Press}}, input.SystemEvent{Event: key.Event{Name: key.NameTab, State: key.Release}})
	r.frame(p)
	r.awaitWrites(pty, "ls", "\r", "\t")
	writes := r.daemon.sent(methodTerminalWrite)
	for i, c := range writes {
		got := params[terminalWriteParams](t, c)
		if c.link.Endpoint != remoteEndpoint || got.TerminalID != "term-1" || got.WriterLease != "lease-1" || got.InputSeq != strconv.Itoa(i+1) {
			t.Fatalf("write %d sent %+v to %s; want term-1 on builder with lease-1 and sequence %d", i+1, got, c.link.Endpoint, i+1)
		}
	}
	if !r.focused(&p.viewport) {
		t.Fatal("Tab inside the terminal moved focus away instead of reaching the program")
	}
	pty.check(t)
}

func TestTerminalCtrlTabLeavesTheViewportWithoutTypingIntoTheProgram(t *testing.T) {
	r, p, pty := terminalRig(t)
	r.startTerminal(p)
	r.focus(p, &p.viewport)
	r.key(p, key.NameTab, key.ModCtrl)
	r.frame(p)
	if r.focused(&p.viewport) || !r.focused(&p.pasteBtn) {
		t.Fatal("Ctrl+Tab must move focus from the terminal to the next panel control")
	}
	r.focus(p, &p.viewport)
	r.key(p, key.NameTab, key.ModCtrl|key.ModShift)
	r.frame(p)
	if r.focused(&p.viewport) || r.focused(&p.pasteBtn) {
		t.Fatal("Ctrl+Shift+Tab must move focus from the terminal to the previous panel control")
	}
	r.key(p, key.NameTab, 0)
	r.frame(p)
	time.Sleep(20 * time.Millisecond)
	if got := pty.written(); len(got) != 0 {
		t.Fatalf("leaving the terminal wrote %q to the program", got)
	}
}

func TestTerminalAppliesAResizeOnlyAtItsOrderedReadPosition(t *testing.T) {
	r, p, pty := terminalRig(t)
	pty.mu.Lock()
	pty.hold = true
	pty.mu.Unlock()
	s := r.startTerminal(p)
	resizes := waitSent(t, r.daemon, methodTerminalResize, 1)
	got := params[terminalResizeParams](t, resizes[0])
	if got.WriterLease != "lease-1" || got.ResizeSeq != "1" || got.Columns == ptyColumns || got.Rows != terminalRows {
		t.Fatalf("resize %+v; want lease-1, sequence 1, and the viewport grid", got)
	}
	eventually(t, "the resize is acknowledged", func() bool {
		pty.mu.Lock()
		defer pty.mu.Unlock()
		return len(pty.resizes) == 1
	})
	time.Sleep(20 * time.Millisecond)
	if columns, _ := s.replica.Size(); columns != ptyColumns {
		t.Fatalf("the acknowledgment alone resized the replica to %d columns", columns)
	}
	pty.mu.Lock()
	pty.hold = false
	pty.mu.Unlock()
	eventually(t, "the read delivers the resize record", func() bool {
		s.mu.Lock()
		defer s.mu.Unlock()
		columns, _ := s.replica.Size()
		_, ordinal := s.replica.Cursors()
		return columns == got.Columns && ordinal == 1
	})
	pty.check(t)
}

func TestTerminalReplayGapRefreshesTheSameAttachmentFromACheckpoint(t *testing.T) {
	r, p, pty := terminalRig(t)
	s := r.startTerminal(p)
	pty.mu.Lock()
	pty.hold = true
	pty.mu.Unlock()
	pty.emit("evicted output\r\n")
	pty.mu.Lock()
	pty.retained, pty.hold = len(pty.out), false
	pty.mu.Unlock()
	eventually(t, "the reader recovers from the gap", func() bool { return s.snapshot().gaps == 1 })
	opens := r.daemon.sent(methodCheckpointOpen)
	if len(opens) != 1 || params[attachmentRef](t, opens[0]) != (attachmentRef{TerminalID: "term-1", AttachmentID: "att-1"}) {
		t.Fatalf("checkpoint/open calls %+v; want one refresh of att-1", opens)
	}
	if n := len(r.daemon.sent(methodTerminalAttach)) + len(r.daemon.sent(methodTerminalOpen)); n != 1 {
		t.Fatalf("recovery allocated another attachment (%d open or attach calls)", n)
	}
	s.mu.Lock()
	next, _ := s.replica.Cursors()
	s.mu.Unlock()
	if next != uint64(len("evicted output\r\n")) {
		t.Fatalf("after recovery the replica continues at %d, want the refreshed checkpoint offset", next)
	}
	r.frame(p)
	r.mustShow("Output gap: older output was no longer retained 1 time(s)")
	pty.check(t)
}

func TestTerminalStopKeepsReadingUntilBothFinalCursorsArrive(t *testing.T) {
	r, p, pty := terminalRig(t)
	s := r.startTerminal(p)
	r.click(p, "Stop terminal")
	r.settle(p, &p.work)
	closes := r.daemon.sent(methodTerminalClose)
	if len(closes) != 1 || closes[0].link.Endpoint != remoteEndpoint || params[terminalCloseParams](t, closes[0]).TerminalID != "term-1" || params[terminalCloseParams](t, closes[0]).OperationID == "" {
		t.Fatalf("terminal/close calls %+v; want one for term-1 on builder with an operation id", closes)
	}
	before := len(r.daemon.sent(methodTerminalRead))
	waitSent(t, r.daemon, methodTerminalRead, before+1)
	if snap := s.snapshot(); !snap.reading || snap.end != endNone {
		t.Fatalf("an exited status without final cursors stopped reading (end %d)", snap.end)
	}
	pty.finalize("bye\r\n")
	eventually(t, "the final output is collected", func() bool { return s.snapshot().end == endCollected })
	r.frame(p)
	r.mustShow("Finished: exited with code 0. All output is collected.")
	if n := len(r.daemon.sent(methodTerminalDetach)); n != 0 {
		t.Fatalf("Stop detached %d times; the reader stays until the user releases it", n)
	}
	pty.check(t)
}

func TestTerminalDetachReleasesTheAttachmentBeforeTheWorkspace(t *testing.T) {
	r, p, pty := terminalRig(t)
	r.startTerminal(p)
	gate := r.daemon.gate(methodTerminalDetach)
	r.click(p, "Detach and close")
	waitSent(t, r.daemon, methodTerminalDetach, 1)
	time.Sleep(50 * time.Millisecond)
	if n := len(r.daemon.sent(methodWorkspaceClose)); n != 0 {
		t.Fatalf("the workspace closed %d times while the detach was still in flight", n)
	}
	close(gate)
	r.settle(p, &p.work)
	r.frame(p)
	if r.closed != 1 {
		t.Fatalf("Detach and close closed the panel %d times", r.closed)
	}
	waitSent(t, r.daemon, methodWorkspaceClose, 1)
	r.daemon.mu.Lock()
	order := slices.Clone(r.daemon.calls)
	r.daemon.mu.Unlock()
	detach := slices.IndexFunc(order, func(c call) bool { return c.method == methodTerminalDetach })
	release := slices.IndexFunc(order, func(c call) bool { return c.method == methodWorkspaceClose })
	if detach < 0 || release < detach {
		t.Fatalf("detach at %d and workspace close at %d; the attachment must go first", detach, release)
	}
	if got := params[attachmentRef](t, order[detach]); got != (attachmentRef{TerminalID: "term-1", AttachmentID: "att-1"}) {
		t.Fatalf("detach %+v; want att-1 of term-1", got)
	}
	if n := len(r.daemon.sent(methodTerminalClose)); n != 0 {
		t.Fatalf("detaching closed the process %d times", n)
	}
	pty.check(t)
}

func TestTerminalCloseDetachesTheAttachment(t *testing.T) {
	r, p, pty := terminalRig(t)
	r.startTerminal(p)
	r.click(p, "Close")
	eventually(t, "the attachment detaches", func() bool {
		r.frame(p)
		return len(r.daemon.sent(methodTerminalDetach)) == 1
	})
	eventually(t, "the panel closes after detach", func() bool {
		r.settle(p, &p.work)
		return r.closed == 1
	})
	if p.session != nil {
		t.Fatal("Close kept the terminal session")
	}
	r.settle(p, &p.work)
	reads := len(r.daemon.sent(methodTerminalRead))
	time.Sleep(20 * time.Millisecond)
	r.frame(p)
	if n := len(r.daemon.sent(methodTerminalRead)); n != reads {
		t.Fatalf("lease reads continued after detach: %d then %d", reads, n)
	}
	if n := len(r.daemon.sent(methodTerminalClose)); n != 0 {
		t.Fatalf("closing the panel stopped the process %d times", n)
	}
	pty.check(t)
}

func TestTerminalHideKeepsItsAttachmentAndLeaseReads(t *testing.T) {
	r, p, pty := terminalRig(t)
	s := r.startTerminal(p)
	r.click(p, "Hide")
	if r.closed != 1 {
		t.Fatal("Hide did not hide the panel")
	}
	before := len(r.daemon.sent(methodTerminalRead))
	waitSent(t, r.daemon, methodTerminalRead, before+1)
	for _, c := range r.daemon.sent(methodTerminalRead) {
		if got := params[terminalReadParams](t, c); got.AttachmentID != "att-1" || got.WaitMs < 1 || got.WaitMs > 1000 {
			t.Fatalf("lease read %+v; want att-1 with a wait inside 1..1000 ms", got)
		}
	}
	if snap := s.snapshot(); !snap.reading || len(r.daemon.sent(methodTerminalDetach)) != 0 {
		t.Fatal("hiding the panel released the terminal")
	}
	pty.check(t)
}

func TestTerminalUncertainWriteStopsFurtherInput(t *testing.T) {
	r, p, pty := terminalRig(t)
	s := r.startTerminal(p)
	pty.mu.Lock()
	pty.writeErr = errTransportLost
	pty.mu.Unlock()
	r.focus(p, &p.viewport)
	r.router.Queue(key.EditEvent{Text: "rm"})
	r.frame(p)
	eventually(t, "the write becomes uncertain", func() bool { return s.snapshot().uncertain })
	r.router.Queue(key.EditEvent{Text: "x"})
	r.frame(p)
	r.frame(p)
	time.Sleep(20 * time.Millisecond)
	if n := len(r.daemon.sent(methodTerminalWrite)); n != 1 {
		t.Fatalf("after an uncertain write the terminal sent %d writes; it must not send more input", n)
	}
	r.mustShow(errInputUncertain.Error())
	if !r.button("Paste").Desc.Disabled {
		t.Fatal("Paste must be disabled while input is uncertain")
	}
	r.button("Stop terminal")
	r.button("New terminal")
}

func TestTerminalReattachesWithItsResumeLeaseAfterReconnect(t *testing.T) {
	r, p, pty := terminalRig(t)
	s := r.startTerminal(p)
	r.focus(p, &p.viewport)
	r.router.Queue(key.EditEvent{Text: "a"})
	r.frame(p)
	r.awaitWrites(pty, "a")
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
	checkReattach(t, r)
	r.frame(p)
	r.focus(p, &p.viewport)
	r.router.Queue(key.EditEvent{Text: "b"})
	r.frame(p)
	r.awaitWrites(pty, "a", "b")
	checkResequenced(t, r)
	pty.check(t)
}

func TestTerminalReaderAttachmentNeverTypes(t *testing.T) {
	r, p, pty := terminalRig(t)
	pty.mu.Lock()
	pty.listing = []terminalListing{{Identity: pty.identity(), Status: pty.status, WriterPresent: true, LeaseExpiresAt: farFuture}}
	pty.mu.Unlock()
	r.click(p, "Refresh running terminals")
	r.settle(p, &p.work)
	r.click(p, "Attach to terminal term-1")
	r.settle(p, &p.work)
	attaches := r.daemon.sent(methodTerminalAttach)
	if len(attaches) != 1 || params[attachParams](t, attaches[0]).Access != "read" {
		t.Fatalf("attach calls %+v; another writer must make this a read attachment", attaches)
	}
	r.frame(p)
	r.mustShow("Running · read only")
	r.focus(p, &p.viewport)
	r.router.Queue(key.EditEvent{Text: "x"})
	r.frame(p)
	time.Sleep(20 * time.Millisecond)
	if n := len(r.daemon.sent(methodTerminalWrite)); n != 0 {
		t.Fatalf("a read attachment sent %d writes", n)
	}
	if n := len(r.daemon.sent(methodTerminalResize)); n != 0 {
		t.Fatalf("a read attachment sent %d resizes", n)
	}
}

type clipboardText string

func (c clipboardText) ReadClipboard(context.Context, platform.PasteKind) (platform.ClipboardContent, error) {
	return platform.ClipboardContent{Text: string(c)}, nil
}

func TestTerminalPasteReadsTheNativeClipboardAndWritesItOnce(t *testing.T) {
	r, p, pty := terminalRig(t)
	r.clipboard = clipboardText("echo hi")
	r.startTerminal(p)
	r.click(p, "Paste")
	r.settle(p, &p.work)
	r.awaitWrites(pty, "echo hi")
	time.Sleep(20 * time.Millisecond)
	if got := pty.written(); len(got) != 1 {
		t.Fatalf("one paste wrote %q", got)
	}
	pty.check(t)
}
