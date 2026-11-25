//go:build gui

package manage

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"fmt"
	"slices"
	"strings"
	"time"

	"gioui.org/layout"
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
	titleLimitBytes = 1024
	archivedPage    = 50
)

var titlePolicy = textinput.Policy{Lines: textinput.SingleLine, Limit: textinput.Limit{Unit: textinput.Bytes, Max: titleLimitBytes}}

func threadTitle(v *state.View, key state.ThreadKey) string {
	if v == nil {
		return key.Thread
	}
	if tv, ok := v.Thread(key); ok {
		return displayTitle(tv.Thread)
	}
	if t, ok := v.Summary(key); ok {
		return displayTitle(t)
	}
	return key.Thread
}

func displayTitle(t rpc.Thread) string {
	switch {
	case t.Name != "":
		return t.Name
	case t.Preview != "":
		return t.Preview
	}
	return t.ID
}

func targetLine(env scene, key state.ThreadKey) layout.Widget {
	return sayer(env, toneDim, fmt.Sprintf("Thread %s on %s", key.Thread, key.Endpoint))
}

type Rename struct {
	base
	target state.ThreadKey
	name   *field
	save   widget.Clickable
	op     mutation
	seeded bool
}

func newRename(h *Hub) *Rename {
	r := &Rename{name: newField(titlePolicy, false)}
	r.init(h)
	return r
}

func (r *Rename) Opened(route shell.Route) {
	r.reopen()
	r.target, r.seeded = route.Thread, false
	r.op.state, r.op.err = outcomeIdle, nil
}

func (r *Rename) Layout(gtx layout.Context, host shell.Env) layout.Dimensions {
	env := r.scene(host)
	r.work.settle()
	b := bind(env.View, r.target.Endpoint)
	current := threadTitle(env.View, r.target)
	if !r.seeded {
		r.name.set(current)
		r.seeded = true
	}
	_, submitted := r.name.update(gtx)
	hint := r.blocked(b, current)
	if (clicked(gtx, &r.save, hint != "") || submitted && hint == "") && !r.op.running() {
		r.submit(b)
	}
	r.hub.checkOutcome(gtx, &r.work, env.View, &r.op, func(jsontext.Value) { r.done() })
	r.flush(env)
	return r.show(gtx, env, page{
		title: "Rename thread",
		body: []layout.Widget{
			targetLine(env, r.target),
			sayer(env, toneBody, "Current title: "+current),
			func(gtx layout.Context) layout.Dimensions {
				return r.name.layout(gtx, env, "New title", r.op.running())
			},
			sayer(env, toneDim, hint),
			r.op.line(env),
		},
		footer: []action{{click: &r.save, label: "Rename", variant: pointed, off: hint != "" || r.op.running()}},
	})
}

func (r *Rename) blocked(b binding, current string) string {
	proposed := r.name.text()
	switch {
	case !b.live:
		return fmt.Sprintf("%s is %s; connect it to rename this thread.", b.endpoint, phaseWord(b.phase))
	case textinput.VisuallyBlank(proposed):
		return "Enter a title."
	case proposed == current:
		return "The title is unchanged."
	}
	return ""
}

func (r *Rename) submit(b binding) {
	id := r.op.begin(methodThreadName, b, false)
	params := rpc.ThreadNameSetParams{ThreadID: r.target.Thread, Name: r.name.text()}
	r.work.spawn(func() func() {
		err := r.hub.mutate(operationRequest{binding: b, deadline: metadataDeadline, method: methodThreadName, operationID: id, params: params, result: nil})
		return func() {
			if r.op.id != id {
				return
			}
			r.op.settle(err)
			if err == nil {
				r.done()
			}
		}
	})
}

func (r *Rename) done() {
	r.announce(platform.AnnounceCompletion, "Thread renamed to "+r.name.text())
	r.finish = true
}

type boxRisk struct {
	loading bool
	loaded  bool
	err     error
	bound   *remoteBinding
	retry   widget.Clickable
	gen     uint64
	thread  string
}

func (k *boxRisk) load(h *Hub, work *tasks, b binding, thread string, gen uint64) {
	k.loading, k.err = true, nil
	k.gen, k.thread = gen, thread
	work.spawn(func() func() {
		var st remoteStatus
		err := h.call(b, metadataDeadline, methodRemoteStatus, remoteStatusParams{ThreadID: thread}, &st)
		return func() {
			if k.gen != gen {
				return
			}
			k.loading, k.loaded, k.err, k.bound = false, err == nil, err, nil
			for i := range st.Bindings {
				if st.Bindings[i].ThreadID == thread {
					k.bound = &st.Bindings[i]
				}
			}
		}
	})
}

func (k *boxRisk) lastBox() bool {
	return k.bound != nil && k.bound.State == "bound" && k.bound.BoxID != "" && len(k.bound.SharedThreadIDs) == 0
}

func (k *boxRisk) text(b binding) (tone, string) {
	switch {
	case b.live && b.features[featureRunnerManagement] < 1:
		return toneWarning, "This daemon cannot report remote runner bindings (runnerManagement:1). If this thread runs on a remote box, archiving its last thread destroys that box and any files not pulled. Upgrade the daemon to see the exact risk."
	case k.loading:
		return toneDim, "Checking the thread's runner binding…"
	case k.err != nil:
		return toneWarning, "Could not check whether archiving destroys a remote box: " + explain(k.err)
	case k.lastBox():
		return toneWarning, fmt.Sprintf("This is the last thread bound to box %s on runner %s. Archiving destroys that box and every file in it that was not pulled. Pull first to keep its changes.", k.bound.BoxID, k.bound.RunnerID)
	case k.bound != nil && k.bound.BoxID != "":
		return toneBody, fmt.Sprintf("Box %s stays for %d other threads.", k.bound.BoxID, len(k.bound.SharedThreadIDs))
	case k.loaded:
		return toneBody, "This thread runs without a remote box; archiving destroys no workspace."
	}
	return toneBody, ""
}

type Archive struct {
	base
	target  state.ThreadKey
	stop    widget.Clickable
	archive widget.Clickable
	risk    boxRisk
	riskGen uint64
	pull    pullRun
	op      mutation
}

func newArchive(h *Hub) *Archive {
	a := &Archive{}
	a.init(h)
	return a
}

func (a *Archive) Opened(route shell.Route) {
	a.reopen()
	a.target = route.Thread
	a.riskGen++
	a.risk = boxRisk{}
	a.pull.reset()
	a.op.state, a.op.err = outcomeIdle, nil
}

func (a *Archive) Layout(gtx layout.Context, host shell.Env) layout.Dimensions {
	env := a.scene(host)
	a.work.settle()
	b := bind(env.View, a.target.Endpoint)
	if b.live && b.features[featureRunnerManagement] >= 1 && !a.risk.loading && !a.risk.loaded && a.risk.err == nil {
		a.risk.load(a.hub, &a.work, b, a.target.Thread, a.riskGen)
	}
	if clicked(gtx, &a.risk.retry, a.risk.loading) {
		a.risk.load(a.hub, &a.work, b, a.target.Thread, a.riskGen)
	}
	running := activeTurn(env.View, a.target) != ""
	if clicked(gtx, &a.stop, !running) {
		env.Submit(state.Interrupt{Window: env.Window, Thread: a.target})
	}
	a.pull.update(gtx, pullWork{hub: a.hub, work: &a.work, view: env.View}, b, a.target.Thread)
	off := a.blocked(b, running) != ""
	if clicked(gtx, &a.archive, off) {
		a.submit(b)
	}
	a.hub.checkOutcome(gtx, &a.work, env.View, &a.op, func(jsontext.Value) { a.done() })
	a.flush(env)
	return a.show(gtx, env, page{title: "Archive thread", body: a.body(env, b, running), footer: a.footer(b, running)})
}

func (a *Archive) body(env scene, b binding, running bool) []layout.Widget {
	riskTone, riskText := a.risk.text(b)
	body := []layout.Widget{
		sayer(env, toneHeading, threadTitle(env.View, a.target)),
		targetLine(env, a.target),
		sayer(env, toneBody, "Archiving hides this thread from the active list. The daemon keeps its journal, and you can restore it from Archived threads."),
		sayer(env, riskTone, riskText),
	}
	if a.risk.err != nil {
		body = append(body, buttons(env, action{click: &a.risk.retry, label: "Check again", variant: secondary}))
	}
	if a.risk.lastBox() {
		body = append(body, a.pull.widgets(env, b, threadCwd(env.View, a.target))...)
	}
	if running {
		body = append(body, sayer(env, toneWarning, "A turn is running. Stop it and wait for it to finish before archiving."))
	}
	return append(body, sayer(env, toneDim, a.blocked(b, running)), a.op.line(env))
}

func (a *Archive) footer(b binding, running bool) []action {
	actions := []action{}
	if running {
		actions = append(actions, action{click: &a.stop, label: "Stop turn", variant: secondary})
	}
	return append(actions, action{click: &a.archive, label: "Archive", variant: pointed, off: a.blocked(b, running) != ""})
}

func (a *Archive) blocked(b binding, running bool) string {
	switch {
	case !b.live:
		return fmt.Sprintf("%s is %s; connect it to archive this thread.", b.endpoint, phaseWord(b.phase))
	case running:
		return "Archive waits until the running turn finishes."
	case a.op.running() || a.pull.op.running():
		return "Wait for the current operation to finish."
	case a.risk.loading:
		return "Wait for the runner check to finish."
	case a.risk.err != nil && b.features[featureRunnerManagement] >= 1:
		return "Check the runner binding again before archiving."
	}
	return ""
}

func (a *Archive) submit(b binding) {
	id := a.op.begin(methodThreadArchive, b, false)
	params := rpc.ThreadArchiveParams{ThreadID: a.target.Thread}
	a.work.spawn(func() func() {
		err := a.hub.mutate(operationRequest{binding: b, deadline: pageDeadline, method: methodThreadArchive, operationID: id, params: params, result: nil})
		return func() {
			if a.op.id != id {
				return
			}
			a.op.settle(err)
			if err == nil {
				a.done()
			}
		}
	})
}

func (a *Archive) done() {
	a.announce(platform.AnnounceCompletion, "Thread archived")
	a.finish = true
}

func activeTurn(v *state.View, key state.ThreadKey) string {
	if v == nil {
		return ""
	}
	if tv, ok := v.Thread(key); ok {
		return tv.ActiveTurn
	}
	return ""
}

func threadCwd(v *state.View, key state.ThreadKey) string {
	if v == nil {
		return ""
	}
	if tv, ok := v.Thread(key); ok {
		return tv.Thread.Cwd
	}
	return ""
}

type pullRun struct {
	op       mutation
	start    widget.Clickable
	cancel   widget.Clickable
	stop     context.CancelFunc
	unlisten func()
	progress *remoteProgress
	result   *remotePull
}

type pullWork struct {
	hub  *Hub
	work *tasks
	view *state.View
}

func (p *pullRun) update(gtx layout.Context, w pullWork, b binding, thread string) {
	if clicked(gtx, &p.start, p.op.running() || !b.live) {
		p.run(w.hub, w.work, b, thread)
	}
	if clicked(gtx, &p.cancel, !p.op.running() || p.stop == nil) {
		p.stop()
	}
	w.hub.checkOutcome(gtx, w.work, w.view, &p.op, func(raw jsontext.Value) {
		var res remotePull
		if json.Unmarshal(raw, &res) == nil {
			p.result = &res
		}
	})
}

func (p *pullRun) reset() {
	if p.stop != nil {
		p.stop()
	}
	if p.unlisten != nil {
		p.unlisten()
	}
	p.stop, p.unlisten, p.progress, p.result = nil, nil, nil, nil
	p.op.state, p.op.err = outcomeIdle, nil
}

func (p *pullRun) run(h *Hub, work *tasks, b binding, thread string) {
	id := p.op.begin(methodRemotePull, b, false)
	p.progress, p.result = nil, nil
	ctx, stop := context.WithTimeout(h.ctx, longDeadline)
	p.stop = stop
	p.unlisten = h.listen(listenKey{endpoint: b.endpoint, method: methodRemoteProgress, id: id}, func(raw jsontext.Value) {
		var ev remoteProgress
		if json.Unmarshal(raw, &ev) == nil {
			work.post(func() {
				if p.op.id == id {
					p.progress = &ev
				}
			})
		}
	})
	work.spawn(func() func() {
		var res remotePull
		err := h.deps.Session.CallOperation(ctx, b.link, rpc.Operation{Method: methodRemotePull, OperationID: id}, threadOnly{ThreadID: thread}, &res)
		return func() {
			stop()
			if p.op.id != id {
				return
			}
			p.unlisten()
			p.stop, p.unlisten = nil, nil
			p.op.settle(err)
			if err == nil {
				p.result = &res
			}
		}
	})
}

func (p *pullRun) widgets(env scene, b binding, cwd string) []layout.Widget {
	dest := cmpOr(cwd, "the thread's working directory")
	out := []layout.Widget{
		sayer(env, toneBody, fmt.Sprintf("Pull applies the box's changes to %s on the %s host. Applying that diff changes the destination workspace. Conflicts stay as three-way conflict markers; nothing is rolled back.", dest, b.endpoint)),
		buttons(env,
			action{click: &p.start, label: "Pull files", variant: secondary, off: p.op.running() || !b.live},
			action{click: p.cancelClick(), label: "Cancel pull", variant: quiet}),
	}
	if p.progress != nil && p.op.running() {
		out = append(out, sayer(env, toneDim, progressText(p.progress.Phase, p.progress.Completed, p.progress.Total)))
	}
	out = append(out, p.op.line(env))
	if p.result != nil {
		out = append(out, pullResult(env, *p.result)...)
	}
	return out
}

func (p *pullRun) cancelClick() *widget.Clickable {
	if !p.op.running() || p.stop == nil {
		return nil
	}
	return &p.cancel
}

func progressText(phase, completed, total string) string {
	if total == "" {
		return fmt.Sprintf("Pull: %s, %s done", phase, completed)
	}
	return fmt.Sprintf("Pull: %s, %s of %s", phase, completed, total)
}

func pullResult(env scene, r remotePull) []layout.Widget {
	out := []layout.Widget{sayer(env, toneBody, fmt.Sprintf("Pull finished with exit code %d: %d changed paths, %d conflicted paths.", r.ExitCode, len(r.ChangedPaths), len(r.ConflictPaths)))}
	if len(r.ChangedPaths) > 0 {
		out = append(out, sayer(env, toneCode, "Changed:\n"+strings.Join(r.ChangedPaths, "\n")))
	}
	if len(r.ConflictPaths) > 0 {
		out = append(out, sayer(env, toneFailed, "Conflicts left in the workspace; resolve them by hand:\n"+strings.Join(r.ConflictPaths, "\n")))
	}
	if r.Truncated {
		out = append(out, sayer(env, toneWarning, "The path lists are truncated at 1000 entries; the outcome above covers every path."))
	}
	if r.Output != "" {
		out = append(out, sayer(env, toneCode, r.Output))
	}
	return out
}

func cmpOr(values ...string) string {
	for _, v := range values {
		if v != "" {
			return v
		}
	}
	return ""
}

type Archived struct {
	base
	endpoint  client.EndpointID
	pick      choice
	filter    *field
	rows      []rpc.Thread
	cursor    string
	loading   bool
	loaded    bool
	err       error
	epoch     uint64
	more      widget.Clickable
	reload    widget.Clickable
	restore   choice
	ops       map[string]*mutation
	restored  []string
	connectTo widget.Clickable
}

func newArchived(h *Hub) *Archived {
	a := &Archived{filter: newField(textinput.Policy{Lines: textinput.SingleLine, Limit: textinput.Limit{Unit: textinput.Bytes, Max: 1024}}, false)}
	a.init(h)
	return a
}

func (a *Archived) Opened(route shell.Route) {
	a.reopen()
	a.endpoint = route.Thread.Endpoint
	a.restart()
	a.restored = nil
}

func (a *Archived) restart() {
	a.epoch++
	a.rows, a.cursor, a.loading, a.loaded, a.err = nil, "", false, false, nil
	a.ops = map[string]*mutation{}
}

func (a *Archived) Layout(gtx layout.Context, host shell.Env) layout.Dimensions {
	env := a.scene(host)
	a.work.settle()
	a.chooseEndpoint(gtx, env)
	b := bind(env.View, a.endpoint)
	a.filter.update(gtx)
	a.updateList(gtx, b)
	if clicked(gtx, &a.connectTo, b.live) {
		env.Submit(state.Connect{Endpoint: a.endpoint})
	}
	visible := a.visible()
	a.updateRestore(gtx, b, visible)
	for _, m := range a.ops {
		a.hub.checkOutcome(gtx, &a.work, env.View, m, nil)
	}
	a.flush(env)
	return a.show(gtx, env, page{title: "Archived threads", body: a.body(env, b, visible), footer: a.footer(b)})
}

func (a *Archived) updateList(gtx layout.Context, b binding) {
	if b.live && !a.loaded && !a.loading && a.err == nil {
		a.load(b)
	}
	if clicked(gtx, &a.more, a.loading || a.cursor == "") {
		a.load(b)
	}
	if clicked(gtx, &a.reload, a.loading || !b.live) {
		a.restart()
		a.load(b)
	}
}

func (a *Archived) updateRestore(gtx layout.Context, b binding, visible []rpc.Thread) {
	if at := a.restore.picked(gtx, len(visible)); at >= 0 {
		a.unarchive(b, visible[at].ID)
	}
}

func (a *Archived) chooseEndpoint(gtx layout.Context, env scene) {
	eps := env.View.Endpoints()
	if a.endpoint == "" && len(eps) > 0 {
		a.endpoint = cmpEndpoint(env.Thread.Endpoint, eps[0].ID)
	}
	if at := a.pick.picked(gtx, len(eps)); at >= 0 && eps[at].ID != a.endpoint {
		a.endpoint = eps[at].ID
		a.restart()
	}
}

func cmpEndpoint(values ...client.EndpointID) client.EndpointID {
	for _, v := range values {
		if v != "" {
			return v
		}
	}
	return ""
}

func (a *Archived) load(b binding) {
	a.loading, a.err = true, nil
	epoch, cursor := a.epoch, a.cursor
	a.work.spawn(func() func() {
		var page rpc.ThreadListResponse
		err := a.hub.call(b, pageDeadline, methodThreadList, rpc.ThreadListParams{Archived: true, Limit: archivedPage, Cursor: cursor}, &page)
		return func() {
			if a.epoch != epoch {
				return
			}
			a.loading, a.err = false, err
			if err != nil {
				return
			}
			a.loaded, a.cursor = true, page.NextCursor
			for _, t := range page.Data {
				if !slices.ContainsFunc(a.rows, func(r rpc.Thread) bool { return r.ID == t.ID }) {
					a.rows = append(a.rows, t)
				}
			}
		}
	})
}

func (a *Archived) visible() []rpc.Thread {
	query := strings.ToLower(a.filter.text())
	if query == "" {
		return a.rows
	}
	out := make([]rpc.Thread, 0, len(a.rows))
	for _, t := range a.rows {
		if strings.Contains(strings.ToLower(displayTitle(t)+" "+t.ID+" "+t.Cwd), query) {
			out = append(out, t)
		}
	}
	return out
}

func (a *Archived) unarchive(b binding, thread string) {
	m := a.ops[thread]
	if m == nil {
		m = &mutation{}
		a.ops[thread] = m
	}
	if m.running() || !b.live {
		return
	}
	id := m.begin(methodThreadUnarchive, b, false)
	epoch := a.epoch
	a.work.spawn(func() func() {
		var res jsontext.Value
		err := a.hub.mutate(operationRequest{binding: b, deadline: pageDeadline, method: methodThreadUnarchive, operationID: id, params: threadOnly{ThreadID: thread}, result: &res})
		return func() {
			if a.epoch != epoch || m.id != id {
				return
			}
			m.settle(err)
			if err == nil {
				a.rows = slices.DeleteFunc(a.rows, func(r rpc.Thread) bool { return r.ID == thread })
				a.restored = append(a.restored, thread)
				a.announce(platform.AnnounceCompletion, "Thread restored to navigation")
			}
		}
	})
}

func (a *Archived) body(env scene, b binding, visible []rpc.Thread) []layout.Widget {
	body := []layout.Widget{
		a.endpointChoice(env),
		func(gtx layout.Context) layout.Dimensions {
			return a.filter.layout(gtx, env, "Filter archived threads", false)
		},
		sayer(env, toneDim, "Unarchive restores the thread to navigation. A remote box destroyed by archiving, and files that were never pulled from it, are not restored."),
	}
	if len(a.restored) > 0 {
		body = append(body, sayer(env, toneAdded, fmt.Sprintf("Restored %d thread(s) on %s.", len(a.restored), a.endpoint)))
	}
	body = append(body, a.status(env, b, len(visible))...)
	a.restore.grow(len(visible))
	for i, t := range visible {
		body = append(body, a.row(env, t, &a.restore.clicks[i]))
	}
	return body
}

func (a *Archived) status(env scene, b binding, shown int) []layout.Widget {
	switch {
	case !b.live:
		return []layout.Widget{
			sayer(env, toneWarning, fmt.Sprintf("%s is %s; connect it to list its archived threads.", a.endpoint, phaseWord(b.phase))),
			buttons(env, action{click: &a.connectTo, label: "Connect " + string(a.endpoint), variant: secondary}),
		}
	case a.err != nil:
		return []layout.Widget{sayer(env, toneFailed, "Archived threads did not load: "+explain(a.err))}
	case a.loading && len(a.rows) == 0:
		return []layout.Widget{sayer(env, toneDim, "Loading archived threads…")}
	case a.loaded && len(a.rows) == 0:
		return []layout.Widget{sayer(env, toneBody, "No archived threads on "+string(a.endpoint)+".")}
	case len(a.rows) > 0 && shown == 0:
		return []layout.Widget{sayer(env, toneBody, "No loaded archived thread matches the filter.")}
	}
	return nil
}

func (a *Archived) row(env scene, t rpc.Thread, restore *widget.Clickable) layout.Widget {
	m := a.ops[t.ID]
	running := m != nil && m.running()
	return func(gtx layout.Context) layout.Dimensions {
		items := []layout.Widget{
			sayer(env, toneHeading, displayTitle(t)),
			sayer(env, toneDim, fmt.Sprintf("%s · %s · updated %s", t.ID, cmpOr(t.Cwd, "no working directory"), unixTime(t.UpdatedAt))),
			buttons(env, action{click: restore, label: "Unarchive " + displayTitle(t), variant: secondary, off: running}),
		}
		if m != nil {
			items = append(items, m.line(env))
		}
		return stack(gtx, theme.Space1, items...)
	}
}

func (a *Archived) endpointChoice(env scene) layout.Widget {
	eps := env.View.Endpoints()
	a.pick.grow(len(eps))
	return func(gtx layout.Context) layout.Dimensions {
		items := make([]layout.Widget, len(eps))
		for i, e := range eps {
			label := fmt.Sprintf("%s · %s", e.ID, phaseWord(e.Phase))
			items[i] = func(gtx layout.Context) layout.Dimensions {
				return option(gtx, env, optionSpec{click: &a.pick.clicks[i], label: label, selected: e.ID == a.endpoint, content: sayer(env, toneBody, label)})
			}
		}
		return flow(gtx, theme.Space2, items...)
	}
}

func (a *Archived) footer(b binding) []action {
	return []action{
		{click: &a.more, label: "Load more", variant: secondary, off: a.loading || a.cursor == ""},
		{click: &a.reload, label: "Reload", variant: quiet, off: a.loading || !b.live},
	}
}

func unixTime(sec int64) string {
	if sec == 0 {
		return "unknown"
	}
	return time.Unix(sec, 0).Format("2006-01-02 15:04")
}
