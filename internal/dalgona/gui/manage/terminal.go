//go:build gui

package manage

import (
	"context"
	"encoding/json/jsontext"
	"fmt"
	"image"
	"image/color"
	"io"
	"strings"

	"gioui.org/f32"
	"gioui.org/font"
	"gioui.org/io/clipboard"
	"gioui.org/io/event"
	"gioui.org/io/key"
	"gioui.org/io/pointer"
	"gioui.org/io/semantic"
	"gioui.org/io/transfer"
	"gioui.org/layout"
	"gioui.org/op/clip"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/terminal"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	terminalRows   = 24
	terminalSize   = 13
	pasteLimit     = 1 << 20
	scrollStep     = 3
	terminalTarget = "the runner's interactive shell: $SHELL or /bin/sh on Unix, %ComSpec% on Windows"
)

var (
	terminalRights = []string{"terminal"}
	keyMap         = map[key.Name]terminal.Key{
		key.NameUpArrow: terminal.KeyUp, key.NameDownArrow: terminal.KeyDown, key.NameRightArrow: terminal.KeyRight, key.NameLeftArrow: terminal.KeyLeft,
		key.NameHome: terminal.KeyHome, key.NameEnd: terminal.KeyEnd, key.NameDeleteForward: terminal.KeyDelete, key.NamePageUp: terminal.KeyPageUp,
		key.NamePageDown: terminal.KeyPageDown, key.NameReturn: terminal.KeyEnter, key.NameEnter: terminal.KeyEnter, key.NameTab: terminal.KeyTab,
		key.NameDeleteBackward: terminal.KeyBackspace, key.NameEscape: terminal.KeyEscape,
		key.NameF1: terminal.KeyF1, key.NameF2: terminal.KeyF2, key.NameF3: terminal.KeyF3, key.NameF4: terminal.KeyF4, key.NameF5: terminal.KeyF5,
		key.NameF6: terminal.KeyF6, key.NameF7: terminal.KeyF7, key.NameF8: terminal.KeyF8, key.NameF9: terminal.KeyF9, key.NameF10: terminal.KeyF10,
		key.NameF11: terminal.KeyF11, key.NameF12: terminal.KeyF12,
	}
	selModes = []struct {
		mode  terminal.SelectionMode
		label string
	}{
		{terminal.SelectCharacters, "characters"},
		{terminal.SelectWords, "words"},
		{terminal.SelectLines, "lines"},
		{terminal.SelectRectangle, "rectangle"},
	}
)

type Terminal struct {
	base
	target      state.ThreadKey
	phase       grantPhase
	lease       lease
	openErr     error
	openBtn     widget.Clickable
	listing     []terminalListing
	listBusy    bool
	listErr     error
	listBtn     widget.Clickable
	attach      choice
	startBtn    widget.Clickable
	starting    mutation
	startGen    uint64
	session     *termSession
	rejoin      *rejoinState
	startErr    error
	stopBtn     widget.Clickable
	stopping    mutation
	detachBtn   widget.Clickable
	hideBtn     widget.Clickable
	detaching   bool
	detachErr   error
	newBtn      widget.Clickable
	copyBtn     widget.Clickable
	pasteBtn    widget.Clickable
	search      *field
	searchQuery string
	searchFound terminal.SearchResult
	searchAt    int
	findNext    widget.Clickable
	selMode     terminal.SelectionMode
	modePick    choice
	painter     terminal.Painter
	view        terminal.View
	top         int64
	follow      bool
	sel         *terminal.Selection
	viewport    int
	cell        image.Point
	asked       image.Point
	notice      string
	copied      string
	pasteAsked  bool
	closeAfter  bool
	retried     client.Link
}

type rejoinState struct {
	offset  uint64
	ordinal uint64
	gaps    int
}

func newTerminal(h *Hub) *Terminal {
	t := &Terminal{follow: true, search: newField(pathPolicy, false)}
	t.init(h)
	return t
}

func (t *Terminal) Opened(route shell.Route) {
	t.reopen()
	if route.Thread == t.target {
		return
	}
	if t.session != nil {
		t.notice = fmt.Sprintf("Terminal %s on %s is still attached; detach it before switching threads.", t.session.identity.TerminalID, t.target.Endpoint)
		return
	}
	t.releaseLease()
	t.startGen++
	t.target, t.phase, t.openErr, t.listing, t.listErr, t.startErr, t.notice = route.Thread, grantClosed, nil, nil, nil, nil, ""
	t.starting.state, t.starting.err = outcomeIdle, nil
	t.stopping.state, t.stopping.err = outcomeIdle, nil
	t.retried = client.Link{}
}

func (t *Terminal) releaseLease() {
	if t.phase == grantOpen {
		t.hub.workspaces.release(t.lease)
	}
	t.phase = grantClosed
}

func (t *Terminal) Layout(gtx layout.Context, host shell.Env) layout.Dimensions {
	env := t.scene(host)
	t.work.settle()
	b := bind(env.View, t.target.Endpoint)
	gate := b.gate(featureTerminal, "interactive terminals")
	t.updateLaunch(gtx, b, gate)
	t.updateSession(gtx, b, env.View)
	if clicked(gtx, &t.hideBtn, false) {
		env.Close()
	}
	if t.closeAfter {
		t.closeAfter = false
		env.Close()
	}
	t.flushClipboard(gtx, env)
	t.flush(env)
	return t.show(gtx, env, page{title: "Terminal", body: t.body(env, b, gate), footer: t.footer(gate), dismiss: func() { t.dismiss(env) }})
}

func (t *Terminal) dismiss(env scene) {
	if t.session == nil {
		env.Close()
		return
	}
	t.detachAndClose()
}

func (t *Terminal) footer(gate string) []action {
	if t.session != nil {
		return []action{
			{click: &t.detachBtn, label: "Detach and close", variant: secondary, off: t.detaching},
			{click: &t.hideBtn, label: "Hide", variant: quiet},
		}
	}
	if t.phase == grantOpen {
		return []action{{click: &t.startBtn, label: "Start terminal", variant: pointed, off: gate != "" || t.starting.running()}}
	}
	return []action{{click: &t.openBtn, label: "Open workspace for a terminal", variant: pointed, off: gate != "" || t.phase == grantOpening}}
}

func (t *Terminal) updateLaunch(gtx layout.Context, b binding, gate string) {
	t.launchGrant(gtx, b, gate)
	t.launchList(gtx, b)
	t.launchStart(gtx, b, gate)
	t.launchAttach(gtx, b)
	t.launchReplace(gtx, b)
}

func (t *Terminal) launchGrant(gtx layout.Context, b binding, gate string) {
	if t.phase == grantOpen && b.link != t.lease.link && t.session == nil {
		t.hub.workspaces.release(t.lease)
		t.phase = grantClosed
	}
	if clicked(gtx, &t.openBtn, gate != "" || t.phase == grantOpening || t.phase == grantOpen) {
		t.acquire(b, nil)
	}
}

func (t *Terminal) launchList(gtx layout.Context, b binding) {
	if clicked(gtx, &t.listBtn, t.phase != grantOpen || t.listBusy) {
		t.list(b)
	}
}

func (t *Terminal) launchStart(gtx layout.Context, b binding, gate string) {
	if clicked(gtx, &t.startBtn, gate != "" || t.phase != grantOpen || t.starting.running() || t.session != nil) {
		t.open(b)
	}
}

func (t *Terminal) launchAttach(gtx layout.Context, b binding) {
	if at := t.attach.picked(gtx, len(t.listing)); at >= 0 && t.session == nil && !t.starting.running() {
		t.attachTo(b, t.listing[at], "", true)
	}
}

func (t *Terminal) launchReplace(gtx layout.Context, b binding) {
	if clicked(gtx, &t.newBtn, t.session == nil || t.starting.running()) {
		t.replace(b)
	}
}

func (t *Terminal) acquire(b binding, then func()) {
	t.phase, t.openErr = grantOpening, nil
	t.hub.workspaces.acquire(b, t.target.Thread, terminalRights, func(l lease, err error) {
		t.work.post(func() {
			if t.phase != grantOpening {
				if err == nil {
					t.hub.workspaces.release(l)
				}
				return
			}
			if err != nil {
				t.phase, t.openErr = grantFailed, err
				return
			}
			t.phase, t.lease = grantOpen, l
			if then != nil {
				then()
				return
			}
			t.list(b)
		})
	})
}

func (t *Terminal) list(b binding) {
	t.listBusy, t.listErr = true, nil
	workspace, gen := t.lease.grant.WorkspaceID, t.startGen
	t.work.spawn(func() func() {
		var res terminalList
		err := t.hub.call(b, metadataDeadline, methodTerminalList, workspaceOnly{WorkspaceID: workspace}, &res)
		return func() {
			if t.startGen != gen {
				return
			}
			t.listBusy, t.listErr = false, err
			if err == nil {
				t.listing = res.Terminals
			}
		}
	})
}

func (t *Terminal) grid() (columns, rows int) {
	columns = terminal.MaxColumns
	if t.cell.X > 0 && t.viewport > 0 {
		columns = t.viewport / t.cell.X
	}
	return max(terminal.MinColumns, min(columns, terminal.MaxColumns)), terminalRows
}

func (t *Terminal) open(b binding) {
	id := t.starting.begin(methodTerminalOpen, b, true)
	gen := t.startGen
	columns, rows := t.grid()
	params := terminalOpenParams{WorkspaceID: t.lease.grant.WorkspaceID, Columns: columns, Rows: rows, OperationID: id}
	l := t.lease
	t.startErr = nil
	t.work.spawn(func() func() {
		var opened terminalOpened
		err := t.hub.call(b, pageDeadline, methodTerminalOpen, params, &opened)
		var s *termSession
		if err == nil {
			s, err = startSession(t.hub, b, l, opened, t.hub.deps.Invalidate)
		}
		return func() { t.started(gen, id, s, err) }
	})
}

func (t *Terminal) started(gen uint64, id string, s *termSession, err error) {
	if t.startGen != gen || t.starting.id != id {
		t.discardStale(s)
		return
	}
	t.starting.settle(err)
	t.startErr = err
	if err != nil {
		t.rejoin = nil
		return
	}
	if old := t.session; old != nil && old != s {
		t.work.spawn(func() func() {
			old.stopWorkers()
			return nil
		})
	}
	t.adoptRejoin(s)
	t.session, t.follow, t.sel, t.asked, t.stopping.state = s, true, nil, image.Point{}, outcomeIdle
	t.announce(platform.AnnounceCompletion, "Terminal attached on "+string(t.target.Endpoint))
}

func (t *Terminal) discardStale(s *termSession) {
	if s == nil {
		return
	}
	go func() { _ = s.detach() }()
}

func (t *Terminal) adoptRejoin(s *termSession) {
	rj := t.rejoin
	t.rejoin = nil
	if rj == nil {
		return
	}
	next, ordinal := s.offsets()
	s.gaps = rj.gaps
	if next > rj.offset || ordinal > rj.ordinal {
		s.gaps++
	}
}

func (t *Terminal) attachTo(b binding, entry terminalListing, resume string, hadWriter bool) {
	access := "write"
	if wantReadAccess(entry, resume, hadWriter) {
		access = "read"
	}
	id := t.starting.begin(methodTerminalAttach, b, false)
	gen := t.startGen
	params := attachParams{WorkspaceID: t.lease.grant.WorkspaceID, TerminalID: entry.Identity.TerminalID, Access: access, ResumeLease: resume}
	l := t.lease
	t.work.spawn(func() func() {
		var opened terminalOpened
		err := t.hub.call(b, pageDeadline, methodTerminalAttach, params, &opened)
		var s *termSession
		if err == nil {
			s, err = startSession(t.hub, b, l, opened, t.hub.deps.Invalidate)
		}
		return func() { t.started(gen, id, s, err) }
	})
}

func wantReadAccess(entry terminalListing, resume string, hadWriter bool) bool {
	if entry.Status.State == terminalStateExit || entry.Status.State == terminalStateClose {
		return true
	}
	if resume != "" {
		return !hadWriter
	}
	return entry.WriterPresent
}

func (t *Terminal) replace(b binding) {
	old := t.session
	snap := old.snapshot()
	if snap.reading && !snap.uncertain && snap.end == endNone {
		return
	}
	t.session = nil
	t.detaching = true
	t.work.spawn(func() func() {
		err := old.detach()
		return func() {
			t.detaching, t.detachErr = false, err
			t.acquire(b, func() { t.open(b) })
		}
	})
}

func (t *Terminal) updateSession(gtx layout.Context, b binding, v *state.View) {
	s := t.session
	if s == nil {
		return
	}
	t.handleInput(gtx, s)
	snap := s.snapshot()
	if clicked(gtx, &t.stopBtn, t.stopping.running() || snap.status.State != "running" && snap.status.State != "") {
		t.stop(b)
	}
	t.hub.checkOutcome(gtx, &t.work, v, &t.stopping, nil)
	t.hub.checkOutcome(gtx, &t.work, v, &t.starting, func(jsontext.Value) { t.list(b) })
	if clicked(gtx, &t.detachBtn, t.detaching) {
		t.detachAndClose()
	}
	if clicked(gtx, &t.copyBtn, t.sel == nil) {
		t.copied = s.selected(*t.sel)
	}
	if clicked(gtx, &t.pasteBtn, !snap.writer || snap.uncertain) {
		t.pasteAsked = true
	}
	t.updateSearch(gtx, s)
	t.updateMode(gtx)
	t.reattach(b, s, snap)
}

func (t *Terminal) reattach(b binding, s *termSession, snap termSnapshot) {
	if snap.end != endLost || !b.live || b.link == s.b.link || b.link == t.retried || t.starting.running() {
		return
	}
	resume := s.attachment.WriterLease
	hadWriter := resume != ""
	entry := terminalListing{Identity: snap.identity, Status: snap.status}
	next, ordinal := s.offsets()
	t.rejoin = &rejoinState{offset: next, ordinal: ordinal, gaps: snap.gaps}
	t.retried = b.link
	t.hub.workspaces.release(s.lease)
	t.phase = grantClosed
	t.acquire(b, func() {
		t.attachTo(b, entry, resume, hadWriter)
	})
}

func (t *Terminal) stop(b binding) {
	s := t.session
	id := t.stopping.begin(methodTerminalClose, b, true)
	params := terminalCloseParams{TerminalID: s.identity.TerminalID, OperationID: id}
	t.work.spawn(func() func() {
		var res terminalClosed
		err := t.hub.call(b, pageDeadline, methodTerminalClose, params, &res)
		return func() {
			if t.stopping.id != id {
				return
			}
			t.stopping.settle(err)
			if err == nil {
				s.mu.Lock()
				s.status = res.Status
				s.mu.Unlock()
				t.announce(platform.AnnounceCompletion, "Terminal stopped on "+string(t.target.Endpoint))
			}
		}
	})
}

func (t *Terminal) detachAndClose() {
	s := t.session
	t.detaching = true
	t.work.spawn(func() func() {
		err := s.detach()
		return func() {
			t.detaching, t.detachErr = false, err
			if t.session == s {
				t.session = nil
			}
			t.phase = grantClosed
			t.closeAfter = err == nil
			t.announce(platform.AnnounceCompletion, "Terminal detached; the process keeps running until it exits or its lease ends")
		}
	})
}

func (t *Terminal) AttachedSession() (terminalID string, thread state.ThreadKey, ok bool) {
	if t.session == nil {
		return "", state.ThreadKey{}, false
	}
	return t.session.identity.TerminalID, t.target, true
}

func (t *Terminal) CloseSessions() {
	s := t.session
	t.session, t.phase = nil, grantClosed
	if s == nil {
		return
	}
	s.stopWorkers()
	t.work.spawn(func() func() {
		err := s.detach()
		return func() {
			t.detachErr = err
		}
	})
}

func (t *Terminal) flushClipboard(gtx layout.Context, env scene) {
	if t.copied != "" {
		gtx.Execute(clipboard.WriteCmd{Type: "application/text", Data: io.NopCloser(strings.NewReader(t.copied))})
		t.copied = ""
		t.notice = "Copied the selection."
	}
	if t.pasteAsked {
		t.pasteAsked = false
		t.requestPaste(gtx, env)
	}
}

func (t *Terminal) requestPaste(gtx layout.Context, env scene) {
	var reader platform.ClipboardReader
	if env.Clipboard != nil {
		reader = env.Clipboard()
	}
	if reader == nil {
		gtx.Execute(clipboard.ReadCmd{Tag: &t.viewport})
		return
	}
	s := t.session
	t.work.spawn(func() func() {
		ctx, cancel := context.WithTimeout(t.hub.ctx, metadataDeadline)
		content, err := reader.ReadClipboard(ctx, platform.PasteOrdinary)
		cancel()
		if err == nil && (len(content.Image) > 0 || len(content.Files) > 0) {
			err = platform.ErrUnsupportedPaste
		}
		return func() {
			if t.session == s && s != nil {
				t.pasteText(s, content.Text, err)
			}
		}
	})
}

func (t *Terminal) body(env scene, b binding, gate string) []layout.Widget {
	body := []layout.Widget{targetLine(env, t.target)}
	if t.notice != "" {
		body = append(body, sayer(env, toneDim, t.notice))
	}
	if t.session != nil {
		return append(body, t.sessionWidgets(env, b)...)
	}
	body = append(body, t.launchInfo(env)...)
	if gate != "" {
		return append(body, sayer(env, toneWarning, gate))
	}
	return append(body, t.launchState(env)...)
}

func (t *Terminal) launchInfo(env scene) []layout.Widget {
	sandbox := "unknown until the thread is open"
	if tv, ok := env.View.Thread(t.target); ok && tv.Sandbox != "" {
		sandbox = tv.Sandbox
	}
	return []layout.Widget{
		sayer(env, toneBody, "Endpoint: "+string(t.target.Endpoint)),
		sayer(env, toneBody, "Shell: "+terminalTarget),
		sayer(env, toneCode, "Workspace root: "+cmpOr(t.lease.grant.Root, threadCwd(env.View, t.target), "unknown")),
		sayer(env, toneBody, "Sandbox: "+sandbox),
		sayer(env, toneDim, "Terminal input and output are raw and unmasked; they never enter the transcript."),
	}
}

func (t *Terminal) launchState(env scene) []layout.Widget {
	switch t.phase {
	case grantClosed:
		return []layout.Widget{sayer(env, toneBody, "Opening the workspace asks you to allow terminal access to this thread's runner.")}
	case grantOpening:
		return []layout.Widget{sayer(env, toneDim, "Waiting for your workspace decision…")}
	case grantFailed:
		return []layout.Widget{sayer(env, toneFailed, openFailure(t.openErr))}
	case grantOpen:
	}
	out := []layout.Widget{t.starting.line(env)}
	if t.startErr != nil {
		out = append(out, sayer(env, toneFailed, "The terminal did not start: "+explain(t.startErr)))
	}
	out = append(out, buttons(env, action{click: &t.listBtn, label: "Refresh running terminals", variant: quiet, off: t.listBusy}))
	if t.listErr != nil {
		out = append(out, sayer(env, toneFailed, "Running terminals did not list: "+explain(t.listErr)))
	}
	t.attach.grow(len(t.listing))
	for i, entry := range t.listing {
		out = append(out, t.listingRow(env, i, entry))
	}
	return out
}

func (t *Terminal) listingRow(env scene, i int, entry terminalListing) layout.Widget {
	access := "write"
	if entry.WriterPresent {
		access = "read only; another attachment is typing"
	}
	label := "Attach to terminal " + entry.Identity.TerminalID
	return func(gtx layout.Context) layout.Dimensions {
		return stack(gtx, theme.Space1,
			sayer(env, toneBody, fmt.Sprintf("Terminal %s · %s · %s", entry.Identity.TerminalID, statusWord(entry.Status), access)),
			buttons(env, action{click: &t.attach.clicks[i], label: label, variant: secondary, off: t.starting.running()}))
	}
}

func statusWord(s terminalStatus) string {
	switch s.State {
	case terminalStateExit:
		if s.ExitCode != nil {
			return fmt.Sprintf("exited with code %d", *s.ExitCode)
		}
		return "exited without an exit code"
	case terminalStateClose:
		return "closed"
	}
	return "running"
}

func (t *Terminal) sessionWidgets(env scene, b binding) []layout.Widget {
	s := t.session
	snap := s.snapshot()
	out := []layout.Widget{
		sayer(env, toneCode, fmt.Sprintf("Terminal %s · runner %s · %s", snap.identity.TerminalID, cmpOr(snap.identity.RunnerID, "local"), snap.identity.Root)),
	}
	out = append(out, t.statusLines(env, b, snap)...)
	if t.startErr != nil {
		out = append(out, sayer(env, toneFailed, "The terminal did not start: "+explain(t.startErr)))
	}
	out = append(out, func(gtx layout.Context) layout.Dimensions { return t.viewportWidget(gtx, env, s, snap) })
	out = append(out, buttons(env, t.sessionActions(snap)...), t.stopping.line(env))
	out = append(out, t.searchWidgets(env, s)...)
	out = append(out, t.modeWidgets(env))
	if t.detachErr != nil {
		out = append(out, sayer(env, toneFailed, "Detach failed: "+explain(t.detachErr)))
	}
	return out
}

func (t *Terminal) sessionActions(snap termSnapshot) []action {
	actions := []action{
		{click: &t.copyBtn, label: "Copy selection", variant: quiet, off: t.sel == nil},
		{click: &t.pasteBtn, label: "Paste", variant: quiet, off: !snap.writer || snap.uncertain},
	}
	if snap.status.State == "running" || snap.status.State == "" {
		actions = append(actions, action{click: &t.stopBtn, label: "Stop terminal", variant: destructive, off: t.stopping.running()})
	}
	if snap.uncertain || snap.end != endNone || snap.status.State != "running" {
		actions = append(actions, action{click: &t.newBtn, label: "New terminal", variant: secondary, off: t.starting.running() || snap.reading && !snap.uncertain})
	}
	return actions
}

func (t *Terminal) statusLines(env scene, b binding, snap termSnapshot) []layout.Widget {
	out := []layout.Widget{sayer(env, statusTone(snap), terminalState(snap, b))}
	out = append(out, t.inputLines(env, snap)...)
	if snap.status.OutputTruncated {
		out = append(out, sayer(env, toneWarning, "Output is truncated: draining stopped before the process's last output arrived."))
	}
	if snap.gaps > 0 {
		out = append(out, sayer(env, toneWarning, fmt.Sprintf("Output gap: older output was no longer retained %d time(s); the screen continues from a fresh checkpoint.", snap.gaps)))
	}
	if snap.dropped > 0 {
		out = append(out, sayer(env, toneDim, fmt.Sprintf("%d oldest scrollback rows were dropped to fit the checkpoint.", snap.dropped)))
	}
	if snap.pending > 0 {
		out = append(out, sayer(env, toneDim, fmt.Sprintf("%d input writes waiting for acknowledgment.", snap.pending)))
	}
	return out
}

func (t *Terminal) inputLines(env scene, snap termSnapshot) []layout.Widget {
	if snap.uncertain {
		return []layout.Widget{sayer(env, toneWarning, errInputUncertain.Error())}
	}
	if snap.inputErr != nil {
		return []layout.Widget{sayer(env, toneFailed, "Input failed: "+explain(snap.inputErr))}
	}
	return nil
}

func (t *Terminal) updateSearch(gtx layout.Context, s *termSession) {
	t.search.update(gtx)
	q := t.search.text()
	if q != t.searchQuery {
		t.searchQuery = q
		t.searchFound, t.searchAt = s.search(q), 0
		if len(t.searchFound.Matches) > 0 {
			t.top, t.follow = t.searchFound.Matches[0].Start.Line, false
		}
	}
	if clicked(gtx, &t.findNext, q == "" || len(t.searchFound.Matches) == 0) {
		t.searchAt = (t.searchAt + 1) % len(t.searchFound.Matches)
		t.top, t.follow = t.searchFound.Matches[t.searchAt].Start.Line, false
	}
}

func (t *Terminal) searchWidgets(env scene, s *termSession) []layout.Widget {
	first, end := s.history()
	matches := len(t.searchFound.Matches)
	return []layout.Widget{
		sayer(env, toneDim, fmt.Sprintf("Search covers retained history: lines %d to %d.", first, end)),
		func(gtx layout.Context) layout.Dimensions {
			return t.search.layout(gtx, env, "Search terminal output", false)
		},
		buttons(env, action{click: &t.findNext, label: fmt.Sprintf("Find next (%d matches)", matches), variant: quiet, off: t.searchQuery == "" || matches == 0}),
	}
}

func (t *Terminal) updateMode(gtx layout.Context) {
	if at := t.modePick.picked(gtx, len(selModes)); at >= 0 {
		t.selMode = selModes[at].mode
		t.sel = nil
	}
}

func (t *Terminal) modeWidgets(env scene) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		t.modePick.grow(len(selModes))
		items := make([]layout.Widget, len(selModes))
		for i, m := range selModes {
			i, m := i, m
			items[i] = func(gtx layout.Context) layout.Dimensions {
				return option(gtx, env, optionSpec{click: &t.modePick.clicks[i], label: "Select by " + m.label, selected: t.selMode == m.mode, content: sayer(env, toneBody, "Select by "+m.label)})
			}
		}
		return stack(gtx, theme.Space2, sayer(env, toneDim, "Selection:"), func(gtx layout.Context) layout.Dimensions { return flow(gtx, theme.Space2, items...) })
	}
}

func statusTone(snap termSnapshot) tone {
	switch snap.end {
	case endFailed, endLost:
		return toneFailed
	case endExpired:
		return toneWarning
	case endNone, endCollected, endReleased:
		return toneBody
	}
	return toneBody
}

func terminalState(snap termSnapshot, b binding) string {
	if text, done := endStateText(snap, b); done {
		return text
	}
	if snap.status.State == terminalStateExit || snap.status.State == terminalStateClose {
		return "Finished: " + statusWord(snap.status) + ". Collecting the last output…"
	}
	if !snap.writer {
		return "Running · read only"
	}
	return "Running · keys go to the terminal while it has focus"
}

func endStateText(snap termSnapshot, b binding) (string, bool) {
	switch snap.end {
	case endLost:
		return fmt.Sprintf("Disconnected from %s; input stopped. The terminal reattaches when %s reconnects within its lease.", b.endpoint, b.endpoint), true
	case endExpired:
		return "Output retention expired; later output is unavailable. Collected output stays here.", true
	case endFailed:
		return "Reading stopped: " + explain(snap.endErr), true
	case endCollected:
		return "Finished: " + statusWord(snap.status) + ". All output is collected.", true
	case endReleased:
		return "Detached.", true
	case endNone:
		return "", false
	}
	return "", false
}

func (t *Terminal) viewportWidget(gtx layout.Context, env scene, s *termSession, snap termSnapshot) layout.Dimensions {
	p := env.Theme.Palette()
	t.painter = terminal.Painter{Shaper: env.Shaper, Palette: terminalPalette(p), Font: font.Font{Typeface: platform.CodeFace}, TextSize: unit.Sp(terminalSize)}
	t.viewport = gtx.Constraints.Max.X
	t.top = s.window(&t.view, t.top, snap.rows, t.follow, t.sel)
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions { return t.painter.Layout(gtx, &t.view) })
	if snap.columns > 0 && len(t.view.Lines) > 0 {
		t.cell = image.Pt(max(1, dims.Size.X/snap.columns), max(1, dims.Size.Y/len(t.view.Lines)))
	}
	t.resizeTo(s, snap)
	defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
	semantic.LabelOp("Terminal " + snap.identity.TerminalID).Add(gtx.Ops)
	event.Op(gtx.Ops, &t.viewport)
	call.Add(gtx.Ops)
	env.owner.frame.Focusable(&t.viewport)
	t.registerTerminal(gtx, s, snap.identity.TerminalID, dims.Size)
	if gtx.Focused(&t.viewport) {
		focusRing(gtx, p.Text, dims.Size, 0)
	}
	return dims
}

func (t *Terminal) registerTerminal(gtx layout.Context, s *termSession, id string, size image.Point) {
	if !access.Enabled(gtx) {
		return
	}
	s.readout().Register(gtx, terminal.OutputRegion{ID: id, Size: size, Tag: &t.viewport, Reveal: func(line int64) {
		t.top, t.follow = line, false
	}})
}

func terminalPalette(p theme.Palette) terminal.Palette {
	return terminal.Palette{
		Foreground: p.Text, Background: p.Canvas, Cursor: p.Text, Selection: p.Tone, Boundary: p.Line,
		ANSI: [16]color.NRGBA{
			p.Canvas, p.FailedText, p.AddedText, p.WarningText, p.Strong, p.AccentText, p.Dim, p.Text,
			p.Faint, p.FailedText, p.AddedText, p.WarningText, p.Strong, p.AccentText, p.Dim, p.Text,
		},
	}
}

func (t *Terminal) resizeTo(s *termSession, snap termSnapshot) {
	columns, rows := t.grid()
	want := image.Pt(columns, rows)
	if !snap.writer || want == t.asked || want == image.Pt(snap.columns, snap.rows) {
		return
	}
	t.asked = want
	s.resize(columns, rows)
}

func (t *Terminal) handleInput(gtx layout.Context, s *termSession) {
	tag := &t.viewport
	mods := key.ModCtrl | key.ModShift | key.ModAlt | key.ModCommand
	filters := []event.Filter{
		key.FocusFilter{Target: tag},
		key.Filter{Focus: tag, Optional: mods},
		key.Filter{Focus: tag, Name: key.NameTab, Optional: key.ModShift},
		key.Filter{Focus: tag, Name: key.NameEscape, Optional: mods},
		transfer.TargetFilter{Target: tag, Type: "application/text"},
		pointer.Filter{Target: tag, Kinds: pointer.Press | pointer.Drag | pointer.Release | pointer.Scroll, ScrollY: pointer.ScrollRange{Min: -1 << 16, Max: 1 << 16}},
	}
	for {
		ev, ok := gtx.Event(filters...)
		if !ok {
			return
		}
		t.dispatch(gtx, s, ev)
	}
}

func (t *Terminal) dispatch(gtx layout.Context, s *termSession, ev event.Event) {
	switch e := ev.(type) {
	case key.FocusEvent:
		t.input(s, s.encodeFocus(e.Focus))
	case key.EditEvent:
		t.input(s, terminal.EncodeText(nil, e.Text, 0))
	case key.Event:
		if !t.leave(gtx, e) {
			t.keyPress(s, e)
		}
	case transfer.DataEvent:
		t.paste(s, e)
	case pointer.Event:
		t.pointer(gtx, s, e)
	}
}

func (t *Terminal) leave(gtx layout.Context, e key.Event) bool {
	if e.Name != key.NameTab || !e.Modifiers.Contain(key.ModCtrl) {
		return false
	}
	step := 1
	if e.Modifiers.Contain(key.ModShift) {
		step = -1
	}
	if e.State == key.Press {
		t.frame.Step(gtx, step)
	}
	return true
}

func (t *Terminal) input(s *termSession, data []byte) {
	if len(data) == 0 {
		return
	}
	if err := s.send(data); err != nil {
		t.notice = err.Error()
	}
}

func (t *Terminal) keyPress(s *termSession, e key.Event) {
	if e.State != key.Press {
		return
	}
	if t.keyShortcut(s, e) {
		return
	}
	mods := terminalModifiers(e.Modifiers)
	if k, ok := keyMap[e.Name]; ok {
		t.follow = true
		t.input(s, s.encodeKey(k, mods))
		return
	}
	if text := keyText(e); text != "" && mods&(terminal.ModCtrl|terminal.ModAlt) != 0 {
		t.follow = true
		t.input(s, terminal.EncodeText(nil, text, mods))
	}
}

func (t *Terminal) keyShortcut(s *termSession, e key.Event) bool {
	switch {
	case e.Modifiers.Contain(key.ModCtrl|key.ModShift) && e.Name == "C":
		if t.sel != nil {
			t.copied = s.selected(*t.sel)
		}
		return true
	case e.Modifiers.Contain(key.ModCtrl|key.ModShift) && e.Name == "V":
		t.pasteAsked = true
		return true
	}
	return false
}

func keyText(e key.Event) string {
	if e.Name == key.NameSpace {
		return " "
	}
	if len([]rune(string(e.Name))) != 1 {
		return ""
	}
	return strings.ToLower(string(e.Name))
}

func terminalModifiers(m key.Modifiers) terminal.Modifiers {
	var out terminal.Modifiers
	if m.Contain(key.ModShift) {
		out |= terminal.ModShift
	}
	if m.Contain(key.ModAlt) {
		out |= terminal.ModAlt
	}
	if m.Contain(key.ModCtrl) {
		out |= terminal.ModCtrl
	}
	return out
}

func (t *Terminal) paste(s *termSession, e transfer.DataEvent) {
	r := e.Open()
	defer r.Close()
	data, err := io.ReadAll(io.LimitReader(r, pasteLimit+1))
	t.pasteText(s, string(data), err)
}

func (t *Terminal) pasteText(s *termSession, text string, err error) {
	switch {
	case err != nil:
		t.notice = "Paste failed: " + err.Error()
	case len(text) > pasteLimit:
		t.notice = "Paste refused: the clipboard holds more than 1 MiB of text."
	default:
		t.input(s, s.encodePaste(text))
	}
}

func (t *Terminal) pointer(gtx layout.Context, s *termSession, e pointer.Event) {
	if e.Kind == pointer.Scroll {
		if s.reportsMouse() {
			at := t.cellAt(e.Position)
			t.input(s, s.encodeMouse(wheelEvent(e, at, s.screenTop())))
			return
		}
		t.scroll(e.Scroll.Y)
		return
	}
	at := t.cellAt(e.Position)
	if s.reportsMouse() {
		t.input(s, s.encodeMouse(mouseEvent(e, at, s.screenTop())))
		return
	}
	t.selectAt(gtx, at, e.Kind)
}

func (t *Terminal) selectAt(gtx layout.Context, at terminal.Point, kind pointer.Kind) {
	switch kind {
	case pointer.Press:
		gtx.Execute(key.FocusCmd{Tag: &t.viewport})
		t.sel = &terminal.Selection{Anchor: at, Head: at, Mode: t.selMode}
	case pointer.Drag:
		if t.sel != nil {
			t.sel.Head = at
		}
	case pointer.Release:
		if t.sel != nil && t.sel.Anchor == t.sel.Head {
			t.sel = nil
		}
	case pointer.Cancel, pointer.Move, pointer.Enter, pointer.Leave, pointer.Scroll:
	}
}

func (t *Terminal) scroll(dy float32) {
	switch {
	case dy < 0:
		t.top -= scrollStep
		t.follow = false
	case dy > 0:
		t.top += scrollStep
		t.follow = t.atBottom()
	}
}

func (t *Terminal) atBottom() bool {
	if t.session == nil {
		return true
	}
	return t.top+int64(len(t.view.Lines)) >= t.session.lineEnd()
}

func (t *Terminal) cellAt(pos f32.Point) terminal.Point {
	if t.cell.X <= 0 || t.cell.Y <= 0 || len(t.view.Lines) == 0 {
		return terminal.Point{}
	}
	row := max(0, min(int(pos.Y)/t.cell.Y, len(t.view.Lines)-1))
	return terminal.Point{Line: t.view.Lines[row].ID, Column: max(0, int(pos.X)/t.cell.X)}
}

func mouseEvent(e pointer.Event, at terminal.Point, screenTop int64) terminal.MouseEvent {
	ev := terminal.MouseEvent{Column: at.Column, Row: max(0, int(at.Line-screenTop)), Button: mouseButton(e.Buttons), Modifiers: terminalModifiers(e.Modifiers)}
	switch e.Kind {
	case pointer.Release:
		ev.Action = terminal.MouseRelease
	case pointer.Drag:
		ev.Action = terminal.MouseMotion
	case pointer.Press, pointer.Cancel, pointer.Move, pointer.Enter, pointer.Leave, pointer.Scroll:
		ev.Action = terminal.MousePress
	}
	return ev
}

func wheelEvent(e pointer.Event, at terminal.Point, screenTop int64) terminal.MouseEvent {
	button := terminal.MouseWheelDown
	if e.Scroll.Y < 0 {
		button = terminal.MouseWheelUp
	}
	return terminal.MouseEvent{Column: at.Column, Row: max(0, int(at.Line-screenTop)), Button: button, Action: terminal.MousePress, Modifiers: terminalModifiers(e.Modifiers)}
}

func mouseButton(b pointer.Buttons) terminal.MouseButton {
	switch {
	case b.Contain(pointer.ButtonSecondary):
		return terminal.MouseRight
	case b.Contain(pointer.ButtonTertiary):
		return terminal.MouseMiddle
	default:
		return terminal.MouseLeft
	}
}
