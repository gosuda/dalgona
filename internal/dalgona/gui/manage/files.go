//go:build gui

package manage

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"fmt"
	"io"
	"io/fs"
	"path"
	"slices"
	"strconv"
	"strings"
	"time"

	"gioui.org/io/clipboard"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	listLimit      = 100
	pathLimitBytes = 4096
	heldRetry      = 5 * time.Second
)

var (
	filesRights = []string{"read", "write"}
	pathPolicy  = textinput.Policy{Lines: textinput.SingleLine, Limit: textinput.Limit{Unit: textinput.Bytes, Max: pathLimitBytes}}
)

type grantPhase uint8

const (
	grantClosed grantPhase = iota
	grantOpening
	grantOpen
	grantFailed
)

type pendingMove uint8

const (
	moveNone pendingMove = iota
	moveClose
	moveTarget
	moveRoute
)

type Files struct {
	base
	target        state.ThreadKey
	phase         grantPhase
	lease         lease
	openErr       error
	routePath     string
	routeErr      error
	openBtn       widget.Clickable
	dir           string
	entries       []fileMetadata
	cursor        string
	revision      string
	listing       bool
	listErr       error
	epoch         uint64
	watch         string
	unlisten      func()
	stale         bool
	rows          choice
	up            widget.Clickable
	refresh       widget.Clickable
	more          widget.Clickable
	goTo          widget.Clickable
	pathEntry     *field
	filter        *field
	newName       *field
	mkfile        widget.Clickable
	mkdir         widget.Clickable
	copyPath      widget.Clickable
	op            mutation
	doc           document
	removing      *entryRef
	renaming      *entryRef
	renameTo      *field
	confirm       widget.Clickable
	keep          widget.Clickable
	renameBtn     widget.Clickable
	removeBtn     widget.Clickable
	renameDir     widget.Clickable
	removeDir     widget.Clickable
	dirMeta       *fileMetadata
	moving        pendingMove
	nextDir       string
	nextFile      *fileMetadata
	nextRoute     shell.Route
	saveFirst     widget.Clickable
	discard       widget.Clickable
	cancelMv      widget.Clickable
	copied        string
	pendingFinish bool
	held          []heldOperation
}

type heldOperation struct {
	lease    lease
	id       string
	method   string
	endpoint client.EndpointID
	checking bool
	retryAt  time.Time
}

func (f *Files) settleOwned(l lease, m *mutation, err error) {
	if uncertain(err) {
		f.held = append(f.held, heldOperation{lease: l, id: m.id, method: m.method, endpoint: m.endpoint})
		return
	}
	f.hub.workspaces.disown(l)
}

func (f *Files) releaseSettled(v *state.View) {
	for i := range f.held {
		f.reconcileHeld(v, &f.held[i])
	}
	f.held = slices.DeleteFunc(f.held, func(h heldOperation) bool { return h.id == "" })
}

func (f *Files) reconcileHeld(v *state.View, h *heldOperation) {
	if h.id == "" {
		return
	}
	if resolvedID(f.op, h.id) || resolvedID(f.doc.op, h.id) {
		f.hub.workspaces.disown(h.lease)
		h.id = ""
		return
	}
	if h.checking || f.hub.deps.Now().Before(h.retryAt) {
		return
	}
	b := bind(v, h.endpoint)
	if !b.live {
		return
	}
	h.checking = true
	id := h.id
	f.work.spawn(func() func() {
		r, err := f.hub.readReceipt(b, id)
		return func() {
			h.checking = false
			if h.id != id {
				return
			}
			if receiptTerminal(r, err) {
				f.hub.workspaces.disown(h.lease)
				h.id = ""
				return
			}
			h.retryAt = f.hub.deps.Now().Add(heldRetry)
		}
	})
}

func resolvedID(m mutation, id string) bool {
	return m.id == id && m.state != outcomeUnknown && !m.checking
}

func receiptTerminal(r receipt, err error) bool {
	if err != nil {
		return false
	}
	switch r.State {
	case "succeeded", "failed", "absent":
		return true
	}
	return false
}

func newFiles(h *Hub) *Files {
	f := &Files{pathEntry: newField(pathPolicy, false), filter: newField(pathPolicy, false), newName: newField(pathPolicy, false), renameTo: newField(pathPolicy, false)}
	f.doc = newDocument()
	f.init(h)
	return f
}

func (f *Files) Opened(route shell.Route) {
	f.reopen()
	f.cancelMove()
	f.routePath, f.routeErr = "", nil
	if route.Thread != f.target && f.doc.dirty() {
		f.moving, f.nextRoute = moveRoute, route
		return
	}
	f.openRoute(route)
}

func (f *Files) openRoute(route shell.Route) {
	f.routePath = route.Context
	if route.Thread == f.target && (f.phase == grantOpen || f.phase == grantOpening || f.doc.dirty()) {
		return
	}
	f.release()
	f.target, f.phase, f.openErr = route.Thread, grantClosed, nil
	f.epoch++
	f.dir, f.entries, f.cursor, f.listErr = "", nil, "", nil
	f.listing, f.dirMeta = false, nil
	f.removing, f.renaming = nil, nil
	f.op.state, f.op.err = outcomeIdle, nil
	f.doc.reset()
}

func (f *Files) release() {
	f.stopWatch()
	if f.phase == grantOpen {
		f.hub.workspaces.release(f.lease)
	}
	f.phase = grantClosed
}

func (f *Files) Layout(gtx layout.Context, host shell.Env) layout.Dimensions {
	env := f.scene(host)
	f.work.settle()
	f.releaseSettled(env.View)
	b := bind(env.View, f.target.Endpoint)
	gate := b.gate(featureWorkspace, "workspace files")
	f.updateOpen(gtx, b, gate)
	f.updateOpenRoute(gtx, b, env, gate)
	routePath := f.routePath
	f.updateMove(gtx, b, env)
	f.settlePendingFinish(env, b)
	f.invalidateStaleRoute(gtx, routePath)
	if b.endpoint != f.target.Endpoint {
		b = bind(env.View, f.target.Endpoint)
		gate = b.gate(featureWorkspace, "workspace files")
	}
	f.emitCopiedNotice(gtx)
	f.flush(env)
	return f.show(gtx, env, page{title: "Files", body: f.body(env, b, gate), footer: f.footer(gate), dismiss: func() {
		f.dismissToClose(env, b)
	}})
}

func (f *Files) updateOpenRoute(gtx layout.Context, b binding, env scene, gate string) {
	if f.phase != grantOpen || gate != "" {
		return
	}
	f.openRouteFile(b)
	if f.moving != moveRoute {
		f.updateBrowser(gtx, b)
	}
	f.doc.update(gtx, f, b)
	f.hub.checkOutcome(gtx, &f.work, env.View, &f.op, func(jsontext.Value) { f.list(b, f.dir, "") })
	f.hub.checkOutcome(gtx, &f.work, env.View, &f.doc.op, func(jsontext.Value) { f.reloadDocument(b) })
}

func (f *Files) settlePendingFinish(env scene, b binding) {
	if !f.pendingFinish {
		return
	}
	f.pendingFinish = false
	if !f.doc.dirty() {
		f.finishMove(f.moving, env, b)
	}
}

func (f *Files) invalidateStaleRoute(gtx layout.Context, routePath string) {
	if routePath != "" || f.routePath == "" {
		return
	}
	gtx.Execute(op.InvalidateCmd{})
}

func (f *Files) emitCopiedNotice(gtx layout.Context) {
	if f.copied == "" {
		return
	}
	gtx.Execute(clipboard.WriteCmd{Type: "application/text", Data: io.NopCloser(strings.NewReader(f.copied))})
	f.announce(platform.AnnounceCompletion, "Copied "+f.copied)
	f.copied = ""
}

func (f *Files) dismissToClose(env scene, b binding) {
	f.cancelMove()
	f.routePath = ""
	if f.doc.dirty() {
		f.moving = moveClose
		return
	}
	f.finishMove(moveClose, env, b)
}

func (f *Files) updateOpen(gtx layout.Context, b binding, gate string) {
	if f.phase == grantOpen && b.link != f.lease.link {
		f.lost()
	}
	if gate == "" && f.phase == grantClosed && f.routePath != "" {
		f.acquire(b)
		return
	}
	if clicked(gtx, &f.openBtn, gate != "" || f.phase == grantOpening || f.phase == grantOpen) {
		f.acquire(b)
	}
}

func (f *Files) lost() {
	f.stopWatch()
	f.hub.workspaces.release(f.lease)
	f.phase, f.openErr = grantFailed, fmt.Errorf("the connection to %s was replaced, so the workspace handle expired; open the workspace again", f.target.Endpoint)
}

func (f *Files) footer(gate string) []action {
	if f.phase == grantOpen {
		return []action{{click: &f.refresh, label: "Refresh", variant: quiet, off: f.listing || gate != ""}}
	}
	return []action{{click: &f.openBtn, label: "Open workspace", variant: pointed, off: gate != "" || f.phase == grantOpening}}
}

func (f *Files) acquire(b binding) {
	f.phase, f.openErr = grantOpening, nil
	epoch := f.epoch
	f.hub.workspaces.acquire(b, f.target.Thread, filesRights, func(l lease, err error) {
		f.work.post(func() {
			if f.epoch != epoch || f.phase != grantOpening {
				if err == nil {
					f.hub.workspaces.release(l)
				}
				return
			}
			if err != nil {
				f.phase, f.openErr = grantFailed, err
				return
			}
			f.phase, f.lease = grantOpen, l
			f.list(b, f.dir, "")
		})
	})
}

func (f *Files) openRouteFile(b binding) {
	requested, ok := f.claimRoutePath()
	if !ok {
		return
	}
	p, err := workspaceRoutePath(f.lease.grant, requested)
	if err != nil {
		f.routeErr = err
		return
	}
	if f.holdRouteFile(requested, p) {
		return
	}
	f.startRouteStat(b, requested, p)
}

func (f *Files) claimRoutePath() (string, bool) {
	if f.routePath == "" {
		return "", false
	}
	requested := f.routePath
	f.routePath = ""
	return requested, true
}

func (f *Files) holdRouteFile(requested, p string) bool {
	dirty := f.doc.dirty()
	if f.doc.path == p && (dirty || f.doc.loading) {
		return true
	}
	if dirty {
		f.moving, f.nextRoute = moveRoute, shell.Route{Thread: f.target, Context: requested}
		return true
	}
	return false
}

func (f *Files) routeStatStale(b binding, epoch uint64, ws string) bool {
	return f.doc.epoch != epoch || f.phase != grantOpen || f.lease.link != b.link || f.lease.grant.WorkspaceID != ws
}

func (f *Files) finishRouteStat(b binding, requested string, target pathParams, meta fileMetadata, err error) {
	if kindOf(err) == rpc.ErrorExpired {
		f.routePath = requested
		f.handleExpiry(err)
	}
	f.statRouteFile(b, target.Path, meta, err)
}

func (f *Files) startRouteStat(b binding, requested, p string) {
	f.doc.reset()
	f.doc.path, f.doc.loading = p, true
	epoch, ws := f.doc.epoch, f.lease.grant.WorkspaceID
	f.work.spawn(func() func() {
		var meta fileMetadata
		err := f.hub.call(b, metadataDeadline, methodWorkspaceStat, pathParams{WorkspaceID: ws, Path: p}, &meta)
		return func() {
			if f.routeStatStale(b, epoch, ws) {
				return
			}
			f.finishRouteStat(b, requested, pathParams{WorkspaceID: ws, Path: p}, meta, err)
		}
	})
}

func (f *Files) statRouteFile(b binding, p string, meta fileMetadata, err error) {
	if err != nil {
		f.doc.loading, f.doc.err = false, err
		return
	}
	if meta.Kind == "directory" {
		f.doc.loading, f.doc.err = false, fmt.Errorf("%q is a folder, not a file", p)
		return
	}
	dir := path.Dir(p)
	if dir == "." {
		dir = ""
	}
	if dir != f.dir {
		f.list(b, dir, "")
	}
	f.doc.load(f, b, p, meta)
}

func slashRoutePaths(g workspaceGrant, p string) (string, string) {
	if g.PathStyle != "windows" {
		return g.Root, p
	}
	return strings.ReplaceAll(g.Root, `\`, "/"), strings.ReplaceAll(p, `\`, "/")
}

func cutWorkspaceRoot(root, p string) (string, bool) {
	rel, ok := strings.CutPrefix(p, strings.TrimRight(root, "/"))
	if root == "" || !ok || !strings.HasPrefix(rel, "/") {
		return "", false
	}
	return rel[1:], true
}

func validWorkspaceRel(p string) bool {
	if p == "." || len(p) > pathLimitBytes || strings.IndexByte(p, 0) >= 0 || workspaceDrivePath(p) {
		return false
	}
	return fs.ValidPath(p)
}

func workspaceRoutePath(g workspaceGrant, p string) (string, error) {
	root, p := slashRoutePaths(g, p)
	if path.IsAbs(p) || workspaceDrivePath(p) {
		rel, ok := cutWorkspaceRoot(root, p)
		if !ok {
			return "", fmt.Errorf("the requested file %q is outside the granted workspace %q", p, g.Root)
		}
		p = rel
	}
	if !validWorkspaceRel(p) {
		return "", fmt.Errorf("the requested path %q does not name a workspace file", p)
	}
	return p, nil
}

func workspaceDrivePath(p string) bool {
	return len(p) >= 2 && p[1] == ':' && p[0]|0x20 >= 'a' && p[0]|0x20 <= 'z'
}

func (f *Files) list(b binding, dir, cursor string) {
	f.listing, f.listErr = true, nil
	if cursor == "" {
		f.epoch++
	}
	epoch, workspace := f.epoch, f.lease.grant.WorkspaceID
	f.work.spawn(func() func() {
		var page listing
		err := f.hub.call(b, pageDeadline, methodWorkspaceList, listParams{WorkspaceID: workspace, Path: dir, Cursor: cursor, Limit: listLimit}, &page)
		return func() {
			if f.epoch != epoch {
				return
			}
			f.listed(b, dir, cursor, page, err)
		}
	})
}

func (f *Files) listed(b binding, dir, cursor string, page listing, err error) {
	f.listing, f.listErr = false, err
	if err != nil {
		f.handleExpiry(err)
		return
	}
	if cursor == "" {
		f.entries = nil
		if dir != f.dir || f.watch == "" {
			f.dir = dir
			f.startWatch(b)
		}
	}
	f.dir, f.cursor, f.revision, f.stale = dir, page.NextCursor, page.DirectoryRevision, false
	f.entries = append(f.entries, page.Entries...)
	f.pathEntry.set(dir)
	if cursor == "" {
		f.statDir(b)
	}
}

func (f *Files) statDir(b binding) {
	f.dirMeta = nil
	if f.dir == "" {
		return
	}
	dir, epoch, ws := f.dir, f.epoch, f.lease.grant.WorkspaceID
	f.work.spawn(func() func() {
		var meta fileMetadata
		err := f.hub.call(b, metadataDeadline, methodWorkspaceStat, pathParams{WorkspaceID: ws, Path: dir}, &meta)
		return func() {
			if err == nil && f.epoch == epoch && f.dir == dir {
				f.dirMeta = &meta
			}
		}
	})
}

func (f *Files) reloadDocument(b binding) {
	if m := f.doc.meta; m != nil {
		f.doc.load(f, b, f.doc.path, *m)
	}
}

func (f *Files) folderAction(c *widget.Clickable) *widget.Clickable {
	if f.dir == "" || f.dirMeta == nil {
		return nil
	}
	return c
}

func (f *Files) handleExpiry(err error) {
	if kindOf(err) == rpc.ErrorExpired {
		f.stopWatch()
		f.hub.workspaces.invalidate(f.lease)
		f.phase, f.openErr = grantFailed, err
	}
}

func (f *Files) startWatch(b binding) {
	f.stopWatch()
	workspace, dir, epoch := f.lease.grant.WorkspaceID, f.dir, f.epoch
	f.work.spawn(func() func() {
		var w watching
		err := f.hub.call(b, pageDeadline, methodWorkspaceWatch, watchParams{WorkspaceID: workspace, Path: dir}, &w)
		return func() {
			if err != nil {
				return
			}
			if f.epoch != epoch || f.phase != grantOpen {
				f.unwatch(b, w.WatchID)
				return
			}
			f.watch = w.WatchID
			f.unlisten = f.hub.listen(listenKey{endpoint: b.endpoint, method: methodWorkspaceChanged, id: w.WatchID}, func(raw jsontext.Value) {
				var change workspaceChange
				if json.Unmarshal(raw, &change) == nil {
					f.work.post(func() { f.changed(b, change) })
				}
			})
		}
	})
}

func (f *Files) changed(b binding, change workspaceChange) {
	if change.WatchID != f.watch || f.phase != grantOpen {
		return
	}
	if change.RescanRequired || change.DirectoryRevision != f.revision {
		f.stale = true
		f.list(b, f.dir, "")
	}
	f.doc.watched(f, b, change)
}

func (f *Files) stopWatch() {
	if f.unlisten != nil {
		f.unlisten()
		f.unlisten = nil
	}
	if f.watch != "" {
		if e, ok := endpointLink(f); ok {
			f.unwatch(e, f.watch)
		}
		f.watch = ""
	}
}

func endpointLink(f *Files) (binding, bool) {
	if f.lease.link.Endpoint == "" {
		return binding{}, false
	}
	return binding{endpoint: f.lease.link.Endpoint, link: f.lease.link, live: true}, true
}

func (f *Files) unwatch(b binding, id string) {
	f.work.spawn(func() func() {
		_ = f.hub.call(b, metadataDeadline, methodWorkspaceUnwatch, watchOnly{WatchID: id}, nil)
		return nil
	})
}

func (f *Files) updateBrowser(gtx layout.Context, b binding) {
	f.pathEntry.update(gtx)
	f.filter.update(gtx)
	f.newName.update(gtx)
	f.renameTo.update(gtx)
	visible := f.visible()
	if at := f.rows.picked(gtx, len(visible)); at >= 0 {
		f.pick(b, visible[at])
	}
	if clicked(gtx, &f.up, f.dir == "" || f.listing) {
		f.navigate(b, path.Dir(f.dir))
	}
	if clicked(gtx, &f.goTo, f.listing) {
		f.navigate(b, strings.Trim(f.pathEntry.text(), "/"))
	}
	if clicked(gtx, &f.refresh, f.listing) {
		f.list(b, f.dir, "")
	}
	if clicked(gtx, &f.more, f.listing || f.cursor == "") {
		f.list(b, f.dir, f.cursor)
	}
	f.updateMutations(gtx, b)
	if clicked(gtx, &f.copyPath, false) {
		f.copied = f.displayPath(f.dir)
	}
}

func (f *Files) navigate(b binding, dir string) {
	f.routeErr = nil
	if dir == "." {
		dir = ""
	}
	if f.doc.dirty() {
		f.moving, f.nextDir, f.nextFile = moveTarget, dir, nil
		return
	}
	f.doc.reset()
	f.list(b, dir, "")
}

func (f *Files) pick(b binding, m fileMetadata) {
	f.routeErr = nil
	full := path.Join(f.dir, m.Name)
	if m.Kind == "directory" {
		f.navigate(b, full)
		return
	}
	if f.doc.path == full && (f.doc.dirty() || f.doc.loading) {
		return
	}
	if f.doc.dirty() {
		f.moving, f.nextFile, f.nextDir = moveTarget, &m, ""
		return
	}
	f.doc.load(f, b, full, m)
}

func (f *Files) cancelMove() {
	f.moving, f.doc.after, f.pendingFinish = moveNone, moveNone, false
	f.nextRoute = shell.Route{}
}

func (f *Files) moveSaveOff(b binding) bool {
	return f.phase != grantOpen || !b.live || b.features[featureWorkspace] < 1 || f.doc.op.running()
}

func (f *Files) updateMove(gtx layout.Context, b binding, env scene) {
	if f.moving == moveNone {
		return
	}
	if clicked(gtx, &f.cancelMv, false) {
		f.cancelMove()
		return
	}
	if clicked(gtx, &f.discard, f.doc.op.running()) {
		f.finishMove(f.moving, env, b)
		return
	}
	if clicked(gtx, &f.saveFirst, f.moveSaveOff(b)) {
		f.doc.after = f.moving
		f.doc.save(f, b, f.doc.path, f.doc.loaded.Revision)
	}
}

func (f *Files) finishMove(move pendingMove, env scene, b binding) {
	f.moving = moveNone
	switch move {
	case moveClose:
		f.doc.reset()
		f.release()
		env.Close()
	case moveTarget:
		next := f.nextFile
		f.doc.reset()
		if next != nil {
			f.doc.load(f, b, path.Join(f.dir, next.Name), *next)
			return
		}
		f.list(b, f.nextDir, "")
	case moveRoute:
		route := f.nextRoute
		f.nextRoute = shell.Route{}
		if route.Thread == f.target {
			f.doc.reset()
			f.routePath = route.Context
			return
		}
		f.openRoute(route)
	case moveNone:
	}
}

func (f *Files) visible() []fileMetadata {
	q := strings.ToLower(f.filter.text())
	if q == "" {
		return f.entries
	}
	return slices.DeleteFunc(slices.Clone(f.entries), func(m fileMetadata) bool { return !strings.Contains(strings.ToLower(m.Name), q) })
}

func (f *Files) displayPath(rel string) string {
	g := f.lease.grant
	sep := "/"
	root := g.Root
	if g.PathStyle == "windows" {
		sep = `\`
		rel = strings.ReplaceAll(rel, "/", sep)
	}
	full := strings.TrimRight(root, sep)
	if rel != "" {
		full += sep + rel
	}
	return fmt.Sprintf("%s: %s", f.target.Endpoint, full)
}

func (f *Files) updateMutations(gtx layout.Context, b binding) {
	busy := f.op.running()
	f.updateCreate(gtx, b, busy)
	f.updateConfirm(gtx, b, busy)
	f.selectTarget(gtx, targetRef{remove: &f.removeBtn, rename: &f.renameBtn, path: f.doc.path, meta: f.doc.meta, dirty: f.doc.dirty()})
	f.selectTarget(gtx, targetRef{remove: &f.removeDir, rename: &f.renameDir, path: f.dir, meta: f.dirMeta, dirty: f.doc.dirty()})
}

func (f *Files) updateCreate(gtx layout.Context, b binding, busy bool) {
	name := f.newName.text()
	if clicked(gtx, &f.mkdir, busy || textinput.VisuallyBlank(name)) {
		f.mutate(b, methodWorkspaceMkdir, mkdirParams{WorkspaceID: f.lease.grant.WorkspaceID, Path: path.Join(f.dir, name), Recursive: false})
	}
	if clicked(gtx, &f.mkfile, busy || textinput.VisuallyBlank(name)) {
		f.doc.create(f, b, path.Join(f.dir, name))
	}
}

func (f *Files) updateConfirm(gtx layout.Context, b binding, busy bool) {
	if clicked(gtx, &f.keep, false) {
		f.removing, f.renaming = nil, nil
	}
	if f.removing != nil && clicked(gtx, &f.confirm, busy) {
		e := *f.removing
		f.removing = nil
		f.leaveTarget(e.path)
		f.mutate(b, methodWorkspaceRemove, removeParams{WorkspaceID: f.lease.grant.WorkspaceID, Path: e.path, ExpectedRevision: e.meta.Revision, Recursive: e.meta.Kind == "directory"})
	}
	if f.renaming != nil && clicked(gtx, &f.confirm, busy || textinput.VisuallyBlank(f.renameTo.text())) {
		e := *f.renaming
		f.renaming = nil
		f.leaveTarget(e.path)
		f.mutate(b, methodWorkspaceRename, renameParams{WorkspaceID: f.lease.grant.WorkspaceID, From: e.path, To: path.Join(path.Dir(e.path), f.renameTo.text()), ExpectedRevision: e.meta.Revision, Replace: false})
	}
}

func (f *Files) leaveTarget(p string) {
	if p != f.dir {
		return
	}
	f.dir, f.dirMeta = strings.TrimPrefix(path.Dir(p), "."), nil
}

type entryRef struct {
	path string
	meta fileMetadata
}

type targetRef struct {
	remove *widget.Clickable
	rename *widget.Clickable
	path   string
	meta   *fileMetadata
	dirty  bool
}

func (f *Files) selectTarget(gtx layout.Context, ref targetRef) {
	off := ref.meta == nil || ref.path == "" || f.op.running() || ref.dirty
	if clicked(gtx, ref.remove, off) {
		f.removing, f.renaming = &entryRef{path: ref.path, meta: *ref.meta}, nil
	}
	if clicked(gtx, ref.rename, off) {
		f.renaming, f.removing = &entryRef{path: ref.path, meta: *ref.meta}, nil
		f.renameTo.set(path.Base(ref.path))
	}
}

func (f *Files) mutate(b binding, method string, params any) {
	id := f.op.begin(method, b, true)
	params = withOperation(params, id)
	l := f.lease
	if !f.hub.workspaces.own(l) {
		f.op.state, f.op.err = outcomeFailed, fmt.Errorf("the workspace handle on %s is gone; open the workspace again", f.target.Endpoint)
		return
	}
	f.work.spawn(func() func() {
		err := f.hub.call(b, pageDeadline, method, params, nil)
		return func() {
			f.settleOwned(l, &f.op, err)
			if f.op.id != id {
				return
			}
			f.op.settle(err)
			f.mutated(b, method, err)
		}
	})
}

func withOperation(params any, id string) any {
	switch p := params.(type) {
	case mkdirParams:
		p.OperationID = id
		return p
	case renameParams:
		p.OperationID = id
		return p
	case removeParams:
		p.OperationID = id
		return p
	}
	return params
}

func (f *Files) mutated(b binding, method string, err error) {
	if err != nil {
		return
	}
	switch method {
	case methodWorkspaceRemove, methodWorkspaceRename:
		f.doc.reset()
	case methodWorkspaceMkdir:
		f.newName.set("")
	}
	f.announce(platform.AnnounceCompletion, "Workspace updated on "+string(f.target.Endpoint))
	f.list(b, f.dir, "")
}

func (f *Files) body(env scene, b binding, gate string) []layout.Widget {
	body := []layout.Widget{targetLine(env, f.target)}
	body = append(body, f.moveWidgets(env, b)...)
	if gate != "" {
		body = append(body, sayer(env, toneWarning, gate))
		if f.doc.path != "" {
			body = append(body, sayer(env, toneDim, "The open file stays read-only on this endpoint until it reconnects; other endpoints stay live."))
		}
		return append(body, f.doc.widgets(env, f, b)...)
	}
	switch f.phase {
	case grantClosed:
		return append(body, sayer(env, toneBody, "Open this thread's workspace with read and write access. The daemon on "+string(f.target.Endpoint)+" asks you to allow it, then browses the thread's runner, local or remote."))
	case grantOpening:
		return append(body, sayer(env, toneDim, "Waiting for your workspace decision on "+string(f.target.Endpoint)+"…"))
	case grantFailed:
		return append(body, sayer(env, toneFailed, openFailure(f.openErr)))
	case grantOpen:
	}
	if f.routeErr != nil {
		body = append(body, sayer(env, toneFailed, "The file did not load: "+explain(f.routeErr)))
	}
	body = append(body, f.browser(env)...)
	return append(body, f.doc.widgets(env, f, b)...)
}

func openFailure(err error) string {
	switch kindOf(err) {
	case rpc.ErrorPermissionDenied:
		return "Denied: the workspace was not authorized. Open it again to ask once more."
	case rpc.ErrorNotFound:
		return "Missing: the thread no longer exists on this endpoint."
	case rpc.ErrorIncompatible:
		return "This thread's runner cannot serve workspace files."
	case rpc.ErrorExpired:
		return "The workspace handle expired; open the workspace again."
	case rpc.ErrorInvalidParams, rpc.ErrorUnavailable, rpc.ErrorStaleRevision, rpc.ErrorLimitExceeded, rpc.ErrorBusy, rpc.ErrorUnknownOutcome, rpc.ErrorReplayGap, rpc.ErrorCancelled, rpc.ErrorOperationConflict:
		return "The workspace did not open: " + explain(err)
	}
	return "The workspace did not open: " + explain(err)
}

func (f *Files) moveWidgets(env scene, b binding) []layout.Widget {
	if f.moving == moveNone {
		return nil
	}
	return []layout.Widget{
		sayer(env, toneWarning, f.doc.path+" has unsaved changes."),
		buttons(env,
			action{click: &f.saveFirst, label: "Save changes", variant: pointed, off: f.moveSaveOff(b)},
			action{click: &f.discard, label: "Discard changes", variant: destructive, off: f.doc.op.running()},
			action{click: &f.cancelMv, label: "Cancel", variant: quiet}),
	}
}

func (f *Files) browser(env scene) []layout.Widget {
	out := []layout.Widget{
		sayer(env, toneCode, f.displayPath(f.dir)),
		buttons(env,
			action{click: &f.up, label: "Parent folder", variant: quiet, off: f.dir == ""},
			action{click: &f.copyPath, label: "Copy path", variant: quiet},
			action{click: f.folderAction(&f.renameDir), label: "Rename this folder", variant: quiet, off: f.op.running()},
			action{click: f.folderAction(&f.removeDir), label: "Remove this folder", variant: destructive, off: f.op.running()}),
		func(gtx layout.Context) layout.Dimensions {
			return f.pathEntry.layout(gtx, env, "Folder path", f.listing)
		},
		buttons(env, action{click: &f.goTo, label: "Go to folder", variant: secondary, off: f.listing}),
		func(gtx layout.Context) layout.Dimensions { return f.filter.layout(gtx, env, "Filter names", false) },
		func(gtx layout.Context) layout.Dimensions {
			return f.newName.layout(gtx, env, "New name", f.op.running())
		},
		buttons(env,
			action{click: &f.mkfile, label: "Create file", variant: secondary, off: f.op.running() || textinput.VisuallyBlank(f.newName.text())},
			action{click: &f.mkdir, label: "Create folder", variant: secondary, off: f.op.running() || textinput.VisuallyBlank(f.newName.text())}),
		f.op.line(env),
	}
	out = append(out, f.confirmWidgets(env)...)
	out = append(out, f.listState(env)...)
	visible := f.visible()
	f.rows.grow(len(visible))
	for i, m := range visible {
		out = append(out, f.entryRow(env, &f.rows.clicks[i], m))
	}
	if f.cursor != "" {
		out = append(out, buttons(env, action{click: &f.more, label: "Load more entries", variant: quiet, off: f.listing}))
	}
	return out
}

func (f *Files) listState(env scene) []layout.Widget {
	switch {
	case f.listErr != nil:
		return []layout.Widget{sayer(env, toneFailed, "The folder did not list: "+explain(f.listErr))}
	case f.listing && len(f.entries) == 0:
		return []layout.Widget{sayer(env, toneDim, "Listing…")}
	case len(f.entries) == 0:
		return []layout.Widget{sayer(env, toneBody, "This folder is empty.")}
	case f.stale:
		return []layout.Widget{sayer(env, toneDim, "The folder changed; refreshing…")}
	}
	return nil
}

func (f *Files) confirmWidgets(env scene) []layout.Widget {
	switch {
	case f.removing != nil:
		e := f.removing
		scope := "this file"
		if e.meta.Kind == "directory" {
			scope = "this folder and everything inside it"
		}
		return []layout.Widget{
			sayer(env, toneWarning, fmt.Sprintf("Remove %s on %s?", scope, f.displayPath(e.path))),
			buttons(env, action{click: &f.confirm, label: "Remove " + path.Base(e.path), variant: destructive, off: f.op.running()}, action{click: &f.keep, label: "Keep", variant: quiet}),
		}
	case f.renaming != nil:
		name := path.Base(f.renaming.path)
		return []layout.Widget{
			func(gtx layout.Context) layout.Dimensions {
				return f.renameTo.layout(gtx, env, "Rename "+name+" to", f.op.running())
			},
			buttons(env, action{click: &f.confirm, label: "Rename " + name, variant: pointed, off: f.op.running()}, action{click: &f.keep, label: "Keep name", variant: quiet}),
		}
	}
	return nil
}

func (f *Files) entryRow(env scene, click *widget.Clickable, m fileMetadata) layout.Widget {
	label := fmt.Sprintf("%s %s", kindMark(m.Kind), m.Name)
	detail := fmt.Sprintf("%s · %s", sizeText(m), cmpOr(m.ModifiedAt, "unknown time"))
	if !m.Writable {
		detail += " · read-only"
	}
	selected := path.Join(f.dir, m.Name) == f.doc.path
	return func(gtx layout.Context) layout.Dimensions {
		return option(gtx, env, optionSpec{click: click, label: label, selected: selected, content: func(gtx layout.Context) layout.Dimensions {
			return flow(gtx, theme.Space3, sayer(env, toneBody, label), sayer(env, toneDim, detail))
		}})
	}
}

func kindMark(kind string) string {
	switch kind {
	case "directory":
		return "Folder"
	case "symlink":
		return "Link"
	case "file":
		return "File"
	}
	return "Other"
}

func sizeText(m fileMetadata) string {
	if m.Kind == "directory" {
		return "folder"
	}
	n, err := strconv.ParseUint(m.Size, 10, 64)
	if err != nil {
		return "unknown size"
	}
	return byteSize(n)
}

func byteSize(n uint64) string {
	switch {
	case n >= 1<<20:
		return fmt.Sprintf("%.1f MiB", float64(n)/(1<<20))
	case n >= 1<<10:
		return fmt.Sprintf("%.1f KiB", float64(n)/(1<<10))
	}
	return fmt.Sprintf("%d bytes", n)
}
