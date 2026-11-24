//go:build gui

package manage

import (
	"context"
	"errors"
	"fmt"

	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const cloudflareRefusal = "cloudflare boxes lose their files when the container stops; use sandbox0, modal, sprites, or docker for fully cloud mode"

var remoteFieldPolicy = inputtext.Policy{Lines: inputtext.SingleLine, Limit: inputtext.Limit{Unit: inputtext.Bytes, Max: 4096}}

type cloudKind uint8

const (
	cloudUp cloudKind = iota + 1
	cloudPull
	cloudDown
)

func (k cloudKind) verb() string {
	switch k {
	case cloudUp:
		return "Cloud up"
	case cloudPull:
		return "Cloud pull"
	case cloudDown:
		return "Box removal"
	}
	return "Cloud operation"
}

type cloudRun struct {
	kind     cloudKind
	id       string
	running  bool
	progress *client.Progress
	err      error
	box      client.CloudBox
	changed  bool
	copied   bool
	folder   string
	stop     context.CancelFunc
}

type endpointRow struct {
	connect    widget.Clickable
	disconnect widget.Clickable
	edit       widget.Clickable
	up         widget.Clickable
	pull       widget.Clickable
	down       widget.Clickable
	confirm    widget.Clickable
	keep       widget.Clickable
	folder     widget.Clickable
	cancel     widget.Clickable
	login      widget.Clickable
	copyAuth   toggle
	asking     cloudKind
	chosen     string
	spec       client.RemoteSpec
	specErr    error
	cloud      cloudRun
	leaving    bool
	leaveErr   error
}

type remoteForm struct {
	open      bool
	editing   string
	name      *field
	daemon    toggle
	url       *field
	tokenFile *field
	service   *field
	image     *field
	template  *field
	save      widget.Clickable
	discard   widget.Clickable
	saving    bool
	err       error
}

func newRemoteForm() remoteForm {
	return remoteForm{
		name: newField(remoteFieldPolicy, false), url: newField(remoteFieldPolicy, false), tokenFile: newField(remoteFieldPolicy, false),
		service: newField(remoteFieldPolicy, false), image: newField(remoteFieldPolicy, false), template: newField(remoteFieldPolicy, false),
	}
}

func (f *remoteForm) fill(name string, spec client.RemoteSpec) {
	f.open, f.editing, f.err = true, name, nil
	f.name.set(name)
	f.daemon.on = spec.Kind == client.RemoteDaemon
	f.url.set(spec.URL)
	f.tokenFile.set(spec.TokenFile)
	f.service.set(spec.Service)
	f.image.set(spec.Image)
	f.template.set(spec.Template)
}

func (f *remoteForm) spec(previous client.RemoteSpec) client.RemoteSpec {
	if f.daemon.on {
		return client.RemoteSpec{Kind: client.RemoteDaemon, URL: f.url.text(), TokenFile: f.tokenFile.text()}
	}
	return client.RemoteSpec{Kind: client.RemoteBox, Service: f.service.text(), Image: f.image.text(), Template: f.template.text(), BoxID: previous.BoxID}
}

type Endpoints struct {
	base
	rows    map[client.EndpointID]*endpointRow
	form    remoteForm
	add     widget.Clickable
	saved   string
	listing []client.EndpointStatus
}

func newEndpoints(h *Hub) *Endpoints {
	e := &Endpoints{rows: map[client.EndpointID]*endpointRow{}, form: newRemoteForm()}
	e.init(h)
	return e
}

func (e *Endpoints) Opened(shell.Route) {
	e.reopen()
	e.saved = ""
}

func (e *Endpoints) row(id client.EndpointID) *endpointRow {
	r := e.rows[id]
	if r == nil {
		r = &endpointRow{}
		e.rows[id] = r
	}
	return r
}

func (e *Endpoints) Layout(gtx layout.Context, host shell.Env) layout.Dimensions {
	env := e.scene(host)
	e.work.settle()
	e.listing = e.hub.deps.Lifecycle.Endpoints()
	for _, s := range e.listing {
		e.updateRow(gtx, env, s)
	}
	e.updateForm(gtx)
	if clicked(gtx, &e.add, e.form.open || len(e.listing) >= client.MaxEndpoints) {
		e.form.fill("", client.RemoteSpec{Kind: client.RemoteBox})
		e.form.editing = ""
	}
	e.flush(env)
	return e.show(gtx, env, page{title: "Endpoints", body: e.body(env), footer: e.footer()})
}

func (e *Endpoints) footer() []action {
	full := len(e.listing) >= client.MaxEndpoints
	return []action{{click: &e.add, label: "Add remote", variant: secondary, off: e.form.open || full}}
}

func (e *Endpoints) body(env scene) []layout.Widget {
	body := []layout.Widget{
		sayer(env, toneDim, fmt.Sprintf("%d of %d endpoints configured; at most %d stay connected at once.", len(e.listing), client.MaxEndpoints, client.MaxLiveEndpoints)),
		sayer(env, toneDim, "A remote daemon or daemon in a box keeps its own journal and threads. A remote execution runner only runs a local thread's tools; manage runners from a thread's Runner panel."),
	}
	if len(e.listing) >= client.MaxEndpoints {
		body = append(body, sayer(env, toneWarning, fmt.Sprintf("The %d-endpoint limit is reached; remove a remote from settings before adding another.", client.MaxEndpoints)))
	}
	if e.saved != "" {
		body = append(body, sayer(env, toneAdded, e.saved))
	}
	if e.form.open {
		body = append(body, e.formWidgets(env)...)
	}
	for _, s := range e.listing {
		body = append(body, e.rowWidget(env, s))
	}
	return body
}

func endpointPhase(env scene, s client.EndpointStatus) (client.Phase, error) {
	if v, ok := endpointView(env.View, s.ID); ok {
		return v.Phase, v.Err
	}
	return s.Phase, nil
}

func liveish(p client.Phase) bool {
	return p == client.PhaseConnected || p == client.PhaseConnecting || p == client.PhaseReconnecting
}

func remoteKindWord(k client.EndpointKind) string {
	switch k {
	case client.EndpointLocal:
		return "Local daemon"
	case client.EndpointBox:
		return "Daemon in a box"
	case client.EndpointDaemon:
		return "Remote daemon"
	case client.EndpointSSH:
		return "SSH daemon"
	case client.EndpointDevice:
		return "Paired device"
	}
	return "Unknown endpoint"
}

func (e *Endpoints) updateRow(gtx layout.Context, env scene, s client.EndpointStatus) {
	r := e.row(s.ID)
	phase, _ := endpointPhase(env, s)
	if clicked(gtx, &r.connect, liveish(phase)) {
		env.Submit(state.Connect{Endpoint: s.ID})
	}
	if clicked(gtx, &r.disconnect, !liveish(phase) || r.leaving) {
		e.disconnect(s.ID, r)
	}
	if clicked(gtx, &r.edit, s.Kind == client.EndpointLocal || e.form.open) {
		e.loadSpec(s.ID, r, true)
	}
	e.updateCloud(gtx, env, s, r)
}

func (e *Endpoints) disconnect(id client.EndpointID, r *endpointRow) {
	r.leaving, r.leaveErr = true, nil
	e.work.spawn(func() func() {
		err := e.hub.deps.Lifecycle.Disconnect(id)
		return func() { r.leaving, r.leaveErr = false, err }
	})
}

func (e *Endpoints) loadSpec(id client.EndpointID, r *endpointRow, edit bool) {
	spec, err := e.hub.deps.Lifecycle.Remote(string(id))
	r.spec, r.specErr = spec, err
	if edit && err == nil {
		e.form.fill(string(id), spec)
	}
}

func (e *Endpoints) updateForm(gtx layout.Context) {
	if !e.form.open {
		return
	}
	for _, f := range []*field{e.form.name, e.form.url, e.form.tokenFile, e.form.service, e.form.image, e.form.template} {
		f.update(gtx)
	}
	e.form.daemon.update(gtx, e.form.saving || e.form.editing != "")
	if clicked(gtx, &e.form.discard, e.form.saving) {
		e.form.open = false
	}
	if clicked(gtx, &e.form.save, e.form.saving) {
		e.save()
	}
}

func (e *Endpoints) save() {
	name := e.form.name.text()
	var previous client.RemoteSpec
	if r := e.rows[client.EndpointID(name)]; r != nil {
		previous = r.spec
	}
	spec := e.form.spec(previous)
	e.form.saving, e.form.err = true, nil
	e.work.spawn(func() func() {
		err := e.hub.deps.Lifecycle.SaveRemote(name, spec)
		return func() {
			e.form.saving, e.form.err = false, err
			if err != nil {
				return
			}
			e.form.open = false
			e.saved = fmt.Sprintf("Saved remote %s. It stays Idle until you connect it.", name)
			e.announce(platform.AnnounceCompletion, e.saved)
		}
	})
}

func (e *Endpoints) formWidgets(env scene) []layout.Widget {
	f := &e.form
	off := f.saving
	title := "Add remote"
	if f.editing != "" {
		title = "Edit remote " + f.editing
	}
	out := []layout.Widget{
		sayer(env, toneHeading, title),
		func(gtx layout.Context) layout.Dimensions {
			return f.name.layout(gtx, env, "Remote name", off || f.editing != "")
		},
		func(gtx layout.Context) layout.Dimensions {
			return f.daemon.layout(gtx, env, "Remote daemon (url and token file) instead of a daemon in a box", off || f.editing != "")
		},
	}
	if f.daemon.on {
		out = append(out,
			func(gtx layout.Context) layout.Dimensions {
				return f.url.layout(gtx, env, "Daemon URL (https or wss)", off)
			},
			func(gtx layout.Context) layout.Dimensions {
				return f.tokenFile.layout(gtx, env, "Token file path", off)
			},
			sayer(env, toneDim, "The token stays in that file; dalgona never shows or copies its contents."))
	} else {
		out = append(out,
			func(gtx layout.Context) layout.Dimensions { return f.service.layout(gtx, env, "Box service", off) },
			func(gtx layout.Context) layout.Dimensions {
				return f.image.layout(gtx, env, "Box image (optional)", off)
			},
			func(gtx layout.Context) layout.Dimensions {
				return f.template.layout(gtx, env, "Box template (optional)", off)
			})
	}
	out = append(out, sayer(env, toneDim, fmt.Sprintf("Save writes [remotes.%s] to this computer's settings; the endpoint starts Idle.", cmpOr(f.name.text(), "<name>"))))
	if f.err != nil {
		out = append(out, sayer(env, toneFailed, "Not saved: "+f.err.Error()))
	}
	return append(out, buttons(env,
		action{click: &f.save, label: "Save remote", variant: pointed, off: off},
		action{click: &f.discard, label: "Discard changes", variant: quiet, off: off}))
}

func (e *Endpoints) rowWidget(env scene, s client.EndpointStatus) layout.Widget {
	r := e.row(s.ID)
	phase, cause := endpointPhase(env, s)
	return func(gtx layout.Context) layout.Dimensions {
		items := []layout.Widget{
			sayer(env, toneHeading, s.Label),
			sayer(env, phaseTone(phase), fmt.Sprintf("%s · %s · %s", phaseWord(phase), remoteKindWord(s.Kind), s.ID)),
		}
		if cause != nil && phase != client.PhaseConnected {
			items = append(items, sayer(env, toneFailed, cause.Error()))
		}
		if r.leaveErr != nil {
			items = append(items, sayer(env, toneFailed, "Disconnect failed: "+r.leaveErr.Error()))
		}
		items = append(items, buttons(env, e.rowActions(s, r, phase)...))
		if s.Kind == client.EndpointBox {
			items = append(items, e.cloudWidgets(env, s, r)...)
		}
		return stack(gtx, theme.Space1, items...)
	}
}

func phaseTone(p client.Phase) tone {
	switch p {
	case client.PhaseOffline:
		return toneWarning
	case client.PhaseFailed, client.PhaseIncompatible:
		return toneFailed
	case client.PhaseIdle, client.PhaseConnecting, client.PhaseConnected, client.PhaseReconnecting:
	}
	return toneDim
}

func (e *Endpoints) rowActions(s client.EndpointStatus, r *endpointRow, phase client.Phase) []action {
	actions := []action{
		{click: &r.connect, label: "Connect " + s.Label, variant: secondary, off: liveish(phase)},
		{click: &r.disconnect, label: "Disconnect " + s.Label, variant: quiet, off: !liveish(phase) || r.leaving},
	}
	if s.Kind != client.EndpointLocal {
		actions = append(actions, action{click: &r.edit, label: "Edit " + s.Label, variant: quiet, off: e.form.open})
	}
	return actions
}

func (e *Endpoints) updateCloud(gtx layout.Context, env scene, s client.EndpointStatus, r *endpointRow) {
	if s.Kind != client.EndpointBox {
		return
	}
	busy := r.cloud.running
	e.updateCloudKind(gtx, s, r, busy)
	r.copyAuth.update(gtx, busy)
	e.updateCloudOps(gtx, env, s, r, busy)
}

func (e *Endpoints) updateCloudKind(gtx layout.Context, s client.EndpointStatus, r *endpointRow, busy bool) {
	for kind, c := range map[cloudKind]*widget.Clickable{cloudUp: &r.up, cloudPull: &r.pull, cloudDown: &r.down} {
		if clicked(gtx, c, busy) {
			r.asking, r.chosen, r.copyAuth.on = kind, "", false
			e.loadSpec(s.ID, r, false)
		}
	}
}

func (e *Endpoints) updateCloudOps(gtx layout.Context, env scene, s client.EndpointStatus, r *endpointRow, busy bool) {
	e.updateCloudPick(gtx, r, busy)
	if clicked(gtx, &r.confirm, busy || !e.confirmable(r)) {
		e.runCloud(s.ID, r)
	}
	if clicked(gtx, &r.cancel, !busy || r.cloud.stop == nil) {
		r.cloud.stop()
	}
	if clicked(gtx, &r.login, busy) {
		env.Open(shell.Route{Panel: "login", Thread: state.ThreadKey{Endpoint: s.ID}})
	}
}

func (e *Endpoints) updateCloudPick(gtx layout.Context, r *endpointRow, busy bool) {
	if clicked(gtx, &r.folder, busy) {
		e.chooseFolder(r)
	}
	if clicked(gtx, &r.keep, busy) {
		r.asking = 0
	}
}

func (e *Endpoints) chooseFolder(r *endpointRow) {
	title := "Choose the workspace to seed the box"
	if r.asking == cloudPull {
		title = "Choose the folder that receives the box's changes"
	}
	e.work.spawn(func() func() {
		path, err := e.hub.chooseFolder(title)
		return func() {
			if err != nil {
				r.cloud.err = err
				return
			}
			if path != "" {
				r.chosen = path
			}
		}
	})
}

func (e *Endpoints) confirmable(r *endpointRow) bool {
	switch {
	case r.specErr != nil || r.spec.Service == "cloudflare":
		return false
	case r.asking == cloudDown:
		return r.spec.BoxID != ""
	}
	return r.chosen != ""
}

func (e *Endpoints) runCloud(id client.EndpointID, r *endpointRow) {
	ctx, stop := context.WithTimeout(e.hub.ctx, longDeadline)
	op := rpc.NewOperationID()
	r.cloud = cloudRun{kind: r.asking, id: op, running: true, stop: stop, folder: r.chosen, copied: r.asking == cloudUp && r.copyAuth.on}
	life := e.hub.deps.Lifecycle
	progress := func(p client.Progress) {
		e.work.post(func() {
			if r.cloud.id == p.OperationID && p.Endpoint == id {
				r.cloud.progress = &p
			}
		})
	}
	req := client.CloudRequest{Endpoint: id, OperationID: op, Progress: progress}
	kind, folder, copyAuth, box := r.asking, r.chosen, r.copyAuth.on, r.spec.BoxID
	r.asking = 0
	e.work.spawn(func() func() {
		defer stop()
		var res cloudRun
		switch kind {
		case cloudUp:
			res.box, res.err = life.CloudUp(ctx, client.CloudUpRequest{CloudRequest: req, Workspace: folder, CopyAuth: copyAuth})
		case cloudPull:
			res.changed, res.err = life.CloudPull(ctx, client.CloudPullRequest{CloudRequest: req, Workspace: folder})
		case cloudDown:
			res.err = life.CloudDown(ctx, client.CloudDownRequest{CloudRequest: req, Box: box})
		}
		return func() {
			if r.cloud.id != op {
				return
			}
			r.cloud.running, r.cloud.stop, r.cloud.err, r.cloud.box, r.cloud.changed = false, nil, res.err, res.box, res.changed
			e.announceCloud(id, r.cloud)
		}
	})
}

func (e *Endpoints) announceCloud(id client.EndpointID, c cloudRun) {
	if c.err != nil {
		e.announce(platform.AnnounceFailure, fmt.Sprintf("%s for %s failed", c.kind.verb(), id))
		return
	}
	e.announce(platform.AnnounceCompletion, fmt.Sprintf("%s for %s finished", c.kind.verb(), id))
}

func (e *Endpoints) cloudWidgets(env scene, s client.EndpointStatus, r *endpointRow) []layout.Widget {
	busy := r.cloud.running
	out := []layout.Widget{buttons(env,
		action{click: &r.up, label: "Cloud up " + s.Label, variant: quiet, off: busy},
		action{click: &r.pull, label: "Cloud pull " + s.Label, variant: quiet, off: busy},
		action{click: &r.down, label: "Remove box of " + s.Label, variant: destructive, off: busy})}
	if r.asking != 0 {
		out = append(out, e.confirmation(env, s, r)...)
	}
	return append(out, e.cloudStatus(env, s, r)...)
}

func (e *Endpoints) confirmation(env scene, s client.EndpointStatus, r *endpointRow) []layout.Widget {
	if r.specErr != nil {
		return []layout.Widget{sayer(env, toneFailed, "The remote configuration could not be read: "+r.specErr.Error())}
	}
	if r.spec.Service == "cloudflare" {
		return []layout.Widget{sayer(env, toneFailed, cloudflareRefusal), buttons(env, action{click: &r.keep, label: "Back", variant: quiet})}
	}
	switch r.asking {
	case cloudUp:
		return e.upConfirmation(env, s, r)
	case cloudPull:
		return []layout.Widget{
			sayer(env, toneBody, fmt.Sprintf("Cloud pull reads the changes in box %s on %s and applies them to the folder you choose on this computer. Applying them changes that folder.", cmpOr(r.spec.BoxID, "(none recorded)"), r.spec.Service)),
			e.folderLine(env, r),
			buttons(env, action{click: &r.confirm, label: "Pull into folder", variant: pointed, off: !e.confirmable(r)}, action{click: &r.keep, label: "Back", variant: quiet}),
		}
	case cloudDown:
		return []layout.Widget{
			sayer(env, toneWarning, fmt.Sprintf("Remove box %s on %s for %s? Removal destroys the remote daemon's journal and every thread stored in that box. Pull first to keep its files.", cmpOr(r.spec.BoxID, "(none recorded)"), r.spec.Service, s.Label)),
			buttons(env, action{click: &r.confirm, label: "Remove box " + r.spec.BoxID, variant: destructive, off: !e.confirmable(r)}, action{click: &r.keep, label: "Keep box", variant: quiet}),
		}
	}
	return nil
}

func (e *Endpoints) upConfirmation(env scene, s client.EndpointStatus, r *endpointRow) []layout.Widget {
	return []layout.Widget{
		sayer(env, toneBody, fmt.Sprintf("Cloud up creates or reuses the box for %s on %s and seeds it from the folder you choose. The daemon then runs inside that box.", s.Label, r.spec.Service)),
		e.folderLine(env, r),
		func(gtx layout.Context) layout.Dimensions {
			return r.copyAuth.layout(gtx, env, "Copy this computer's provider credentials into the box", r.cloud.running)
		},
		buttons(env, action{click: &r.confirm, label: "Start box", variant: pointed, off: !e.confirmable(r)}, action{click: &r.keep, label: "Back", variant: quiet}),
	}
}

func (e *Endpoints) folderLine(env scene, r *endpointRow) layout.Widget {
	label := "No folder chosen"
	if r.chosen != "" {
		label = "Folder: " + r.chosen
	}
	return func(gtx layout.Context) layout.Dimensions {
		return flow(gtx, theme.Space3, sayer(env, toneCode, label), buttons(env, action{click: &r.folder, label: "Choose folder", variant: secondary}))
	}
}

func (e *Endpoints) cloudStatus(env scene, s client.EndpointStatus, r *endpointRow) []layout.Widget {
	c := r.cloud
	switch {
	case c.running:
		text := c.kind.verb() + " is starting"
		if c.progress != nil {
			text = fmt.Sprintf("%s: %s (%d of %d)", c.kind.verb(), c.progress.Phase, c.progress.Done, c.progress.Total)
		}
		return []layout.Widget{sayer(env, toneDim, text), buttons(env, action{click: &r.cancel, label: "Cancel " + c.kind.verb(), variant: quiet})}
	case c.err != nil:
		return []layout.Widget{sayer(env, toneFailed, cloudFailure(c))}
	case c.kind == cloudUp:
		return e.upResult(env, s, r)
	case c.kind == cloudPull && c.changed:
		return []layout.Widget{sayer(env, toneAdded, "Cloud pull applied the box's changes to "+c.folder+".")}
	case c.kind == cloudPull:
		return []layout.Widget{sayer(env, toneBody, "Cloud pull found no changes in the box.")}
	case c.kind == cloudDown:
		return []layout.Widget{sayer(env, toneBody, "The box was removed and its record cleared.")}
	}
	return nil
}

func cloudFailure(c cloudRun) string {
	switch {
	case errors.Is(c.err, client.ErrBoxChanged):
		return "The box changed after you confirmed; nothing was removed. Review the new box and confirm again."
	case errors.Is(c.err, context.Canceled):
		return c.kind.verb() + " stopped on request. Steps that already finished stay done; check the box before retrying."
	}
	return c.kind.verb() + " failed: " + c.err.Error()
}

func (e *Endpoints) upResult(env scene, s client.EndpointStatus, r *endpointRow) []layout.Widget {
	out := []layout.Widget{sayer(env, toneAdded, fmt.Sprintf("Box %s on %s is ready for %s.", r.cloud.box.BoxID, r.cloud.box.Service, s.Label))}
	if !r.cloud.copied {
		out = append(out,
			sayer(env, toneBody, "No credentials were copied. Sign in to a provider on that endpoint to start turns there."),
			buttons(env, action{click: &r.login, label: "Sign in on " + s.Label, variant: secondary}))
	}
	return out
}
